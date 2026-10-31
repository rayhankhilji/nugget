//! Restore: verify a backup and stage it under `<data>/restore-pending/`, then apply the staged
//! restore when the store is next opened (daemon start, or right away for the offline CLI).
//!
//! **Staging** never touches the live data. It checks the SHA-256 of every file in the manifest
//! (`key.json` is read once and the verified bytes are the ones kept), that the key opens the
//! copy (the store key when its id matches the manifest, else the key sealed in `key.json`
//! opened with the passphrase), SQLite's full `integrity_check`, and a full
//! [`Store::check_consistency`] (audit chain, search and vector indexes) on a scratch copy. It
//! writes `ready.json` last and renames the staging directory into place.
//!
//! **Applying** ([`apply_pending`]) runs these steps, each skipped once done, so a crash at any
//! point is finished by the next start, and the home stays openable throughout:
//! 1. *Database*: the current database and its `-wal`/`-shm`/`-journal` are moved aside
//!    (`nugget.db.pre-restore-<ts>`, or `nugget.db.corrupt-<ts>` when it was damaged; side files
//!    first, so the restored database never meets an old WAL), then the staged one is moved in.
//!    A failed rename puts back what was already moved.
//! 2. *Key* (only when the backup's key differs): [`StoreKey::install`] (which keeps a different
//!    old key as `store.key.replaced-<ts>`). If that fails, step 1 is undone, so the old database
//!    and the old key stay together. In a home with no key at all (offline restore into an empty
//!    home) the key is installed first instead: there is no old database it could orphan.
//! 3. *Config*: the backup's `config.toml` replaces the current one (kept as
//!    `config.toml.pre-restore-<ts>`) once it is valid **on this computer**: a `[vault] path`
//!    that is not an absolute path here (a backup from another OS) is commented out, so the
//!    default vault is used. A config that still does not load is not installed: it is kept as
//!    `config.toml.restored-<ts>` and the current one stays. `backup.json` is only taken when
//!    this computer has none.
//! 4. *Vault*: the backup's vault goes into the vault directory of the config now in place,
//!    only if that is missing or empty: copied into a temporary sibling, then renamed into place,
//!    and marked done.
//! 5. *Passphrase envelope* (key changed): the backup's `key.json` (same passphrase as the
//!    backup) replaces `backup-key.json` unless that already seals the restored key, so later
//!    backups stay restorable; the old one is kept as `backup-key.json.pre-restore-<ts>`.
//!
//! `ready.json` then becomes `applied.json`; [`finish_applied`] opens the store, re-anchors the
//! audit log and records the restore in it, and removes the staging directory. Nothing is ever
//! deleted except the staging directory itself (and an abandoned `restore-pending.partial`).
//!
//! When the backup's key is not the current store key, staging keeps it until the next start in
//! `restore-pending/staged-key.json`, sealed under a random token stored in the owner-only
//! `ready.json` beside it: no stronger than the `0600` key file used when there is no keychain,
//! and it exists only between staging and the next start. (On Windows the files rely on the
//! data directory's ACL.)
//!
//! Test hook: `NUGGET_RESTORE_TEST_STOP_AFTER=<step>` (`key-first`, `db`, `key`, `config`,
//! `vault-copied`, `vault`, `envelope`) makes applying fail right after that step, as a crash
//! there would leave it. Test-only; never set it in a real install.

use std::fs;
use std::path::{Path, PathBuf};

use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_store::{AuditEntry, FileHealth, KeyEnvelope, Store, StoreKey};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::disk::{
    self, FileEntry, KEY_FILE, MANIFEST_FILE, Manifest, STORE_FILE, compact, io_err, rfc3339,
    sha256_file, write_private,
};
use crate::config::Config;

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
/// Marks the vault step done.
const VAULT_DONE: &str = "vault.done";
/// Cheapest Argon2 cost `seal_with` allows, for [`STAGED_KEY`]: the token is a random 256-bit
/// secret, not a passphrase.
const TOKEN_KDF: (u32, u32, u32) = (8 * 1024, 1, 1);
/// Test-only: fail applying after the named step (see the module docs).
const TEST_STOP_ENV: &str = "NUGGET_RESTORE_TEST_STOP_AFTER";

/// `<data>/restore-pending`.
pub(crate) fn pending_dir(paths: &Paths) -> PathBuf {
    paths.data.join(PENDING)
}

fn partial_dir(paths: &Paths) -> PathBuf {
    paths.data.join(format!("{PENDING}.partial"))
}

/// Whether a verified restore waits for the next start.
pub(crate) fn is_pending(paths: &Paths) -> bool {
    pending_dir(paths).join(READY).is_file()
}

/// Remove a staging directory left by a restore that was interrupted while staging (call while
/// holding the daemon's socket, so nothing is staging meanwhile).
pub(crate) fn sweep_partial(paths: &Paths) {
    let partial = partial_dir(paths);
    if partial.exists() {
        tracing::info!(dir = %partial.display(), "removing an interrupted restore staging folder");
        remove_tree(&partial);
    }
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
    /// The vault directory used when the config sets no `[vault] path` (the daemon's default).
    pub default_vault: Option<String>,
    /// Opens [`STAGED_KEY`]; set exactly when the backup's key is not the store key the home
    /// had when staging (so the key, and the passphrase envelope, change).
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
    /// The daemon's default vault directory (see [`Ready::default_vault`]).
    pub default_vault: Option<&'a Path>,
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
    // key.json: read once, checked, and these very bytes are used and kept. store.nbk is
    // checked on the staged copy (what is restored is what is hashed).
    let key_path = dir.join(KEY_FILE);
    let envelope_bytes = fs::read(&key_path).map_err(|e| io_err(&key_path, e))?;
    let got = FileEntry {
        bytes: envelope_bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&envelope_bytes)),
    };
    if got != manifest.files[KEY_FILE] {
        return Err(Error::Integrity(format!(
            "{KEY_FILE} in {} does not match its manifest checksum (damaged or modified)",
            dir.display()
        )));
    }
    let key = backup_key(&manifest, dir, &envelope_bytes, req.current, req.passphrase)?;

    let root = pending_dir(paths);
    let partial = partial_dir(paths);
    crate::fsutil::create_private_dir(&paths.data).map_err(|e| io_err(&paths.data, e))?;
    remove_tree(&partial);
    crate::fsutil::create_private_dir(&partial).map_err(|e| io_err(&partial, e))?;
    let staged = (|| {
        let db = partial.join(STAGED_DB);
        fs::copy(dir.join(STORE_FILE), &db).map_err(|e| io_err(&dir.join(STORE_FILE), e))?;
        let got = sha256_file(&db)?;
        if got != manifest.files[STORE_FILE] {
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
        // Paths are validated by `read_backup_files` (writable on every OS, no clashes).
        let files = nugget_store::read_backup_files(&db, &key)?;
        nugget_store::strip_backup_files(&db, &key)?;
        check_scratch(&partial, &db, &key)?;
        for (rel, content) in &files {
            let path = join_rel(&partial.join(STAGED_FILES), rel);
            if let Some(parent) = path.parent() {
                crate::fsutil::create_private_dir(parent).map_err(|e| io_err(parent, e))?;
            }
            fs::write(&path, content).map_err(|e| io_err(&path, e))?;
        }
        write_private(&partial.join(STAGED_ENVELOPE), &envelope_bytes)?;
        let same_key = req.current.is_some_and(|k| k.key_id() == key.key_id());
        let key_token = if same_key {
            None
        } else {
            let mut raw = zeroize::Zeroizing::new([0u8; 32]);
            getrandom::fill(raw.as_mut())
                .map_err(|e| Error::Other(format!("no OS randomness: {e}")))?;
            let token = hex::encode(raw.as_ref());
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
            default_vault: req.default_vault.map(|p| p.display().to_string()),
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

/// The key that opens the backup: the current store key when its id matches, else the one
/// sealed in `envelope` (the verified bytes of `key.json`; needs the passphrase).
fn backup_key(
    manifest: &Manifest,
    dir: &Path,
    envelope: &[u8],
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
    let env: KeyEnvelope = serde_json::from_slice(envelope)
        .map_err(|e| Error::Integrity(format!("{KEY_FILE} in {}: {e}", dir.display())))?;
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

/// Fail as a crash right after `step` would, when the test hook asks for it.
fn checkpoint(step: &str) -> Result<()> {
    if std::env::var(TEST_STOP_ENV).is_ok_and(|v| v == step) {
        return Err(Error::Other(format!(
            "test: stopped after restore step {step}"
        )));
    }
    Ok(())
}

/// Whether the key stored for this home (keychain or key file) is `key_id`.
fn installed_is(paths: &Paths, key_id: &str) -> Result<bool> {
    Ok(StoreKey::load_existing(paths)?.is_some_and(|k| k.key_id() == key_id))
}

/// Apply a staged restore, if one is pending (see the module docs). Returns the key to open
/// the restored store with and what was done; `None` when nothing is pending. `current` is the
/// key the store would otherwise be opened with. Blocking.
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
    // Decided at staging, so a repeated apply after a crash makes the same choices.
    let key_changes = ready.key_token.is_some();
    let db = paths.db_file();
    let aside = sibling(&db, &format!("{}-{}", ready.aside.label(), ready.stamp));

    // No key at all here: nothing the new key could orphan, and a daemon could not even start
    // (its database would exist without a key) if we stopped before installing it.
    if key_changes && current.is_none() && !installed_is(paths, &ready.key_id)? {
        install_key(paths, &key)?;
        checkpoint("key-first")?;
    }

    // 1. Database.
    swap_db(paths, &root.join(STAGED_DB), &db, &aside)?;
    checkpoint("db")?;

    // 2. Key; undo step 1 if it cannot be installed, so the old database and key stay together.
    let key = if key_changes && !installed_is(paths, &ready.key_id)? {
        if let Err(e) = install_key(paths, &key) {
            undo_swap(&root.join(STAGED_DB), &db, &aside);
            return Err(e);
        }
        StoreKey::load_existing(paths)?
            .filter(|k| k.key_id() == key.key_id())
            .unwrap_or(key)
    } else if key_changes {
        StoreKey::load_existing(paths)?
            .filter(|k| k.key_id() == key.key_id())
            .unwrap_or(key)
    } else {
        key
    };
    checkpoint("key")?;

    // 3. Settings.
    let files = root.join(STAGED_FILES);
    restore_config(paths, &files, &ready.stamp)?;
    checkpoint("config")?;

    // 4. Vault.
    restore_vault(paths, &root, &ready)?;
    checkpoint("vault")?;

    // 5. Passphrase envelope.
    if key_changes {
        let live = disk::envelope_path(paths);
        let current_env = disk::load_envelope(paths).ok().flatten();
        if current_env
            .as_ref()
            .is_none_or(|e| e.key_id() != ready.key_id)
        {
            if live.exists() {
                let to = sibling(&live, &format!("pre-restore-{}", ready.stamp));
                if !to.exists() {
                    fs::rename(&live, &to).map_err(|e| io_err(&live, e))?;
                }
            }
            let staged = root.join(STAGED_ENVELOPE);
            let env = fs::read(&staged).map_err(|e| io_err(&staged, e))?;
            write_private(&live, &env)?;
        }
    }
    checkpoint("envelope")?;

    fs::rename(&ready_path, root.join(APPLIED)).map_err(|e| io_err(&ready_path, e))?;
    disk::sync_dir(&root);
    let moved_to = aside.exists().then_some(aside);
    Ok(Some((key, Applied { ready, moved_to })))
}

/// [`StoreKey::install`], logged. Errors (for example a keychain that is enabled but cannot be
/// read: `Error::Storage`) are returned as they are.
fn install_key(paths: &Paths, key: &StoreKey) -> Result<()> {
    let source = StoreKey::install(paths, key)?;
    tracing::info!(?source, "restored store key installed");
    Ok(())
}

/// Step 1: move `db` and its side files to `aside` (side files first), then `staged` to `db`.
/// Skipped when `staged` is gone (done before). On a failed rename, what was moved is put back.
fn swap_db(paths: &Paths, staged: &Path, db: &Path, aside: &Path) -> Result<()> {
    if !staged.exists() {
        return Ok(());
    }
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let result = (|| {
        for suffix in ["-wal", "-shm", "-journal", ""] {
            let from = suffixed(db, suffix);
            let to = suffixed(aside, suffix);
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
                moved.push((from, to));
            }
        }
        disk::sync_dir(&paths.data);
        fs::rename(staged, db).map_err(|e| io_err(db, e))?;
        disk::sync_dir(&paths.data);
        Ok(())
    })();
    if result.is_err() {
        for (from, to) in moved.iter().rev() {
            if let Err(e) = fs::rename(to, from) {
                tracing::error!(from = %to.display(), to = %from.display(), error = %e, "cannot put the previous database back");
            }
        }
        disk::sync_dir(&paths.data);
    }
    result
}

/// Undo [`swap_db`]: the restored database back to staging, the previous one back in place.
fn undo_swap(staged: &Path, db: &Path, aside: &Path) {
    if !staged.exists()
        && let Err(e) = fs::rename(db, staged)
    {
        tracing::error!(error = %e, "cannot move the restored database back to staging");
        return;
    }
    for suffix in ["", "-journal", "-shm", "-wal"] {
        let (from, to) = (suffixed(aside, suffix), suffixed(db, suffix));
        if from.exists()
            && !to.exists()
            && let Err(e) = fs::rename(&from, &to)
        {
            tracing::error!(from = %from.display(), error = %e, "cannot put the previous database back");
        }
    }
}

/// Step 3: `config.toml` (made valid for this computer, see the module docs) and `backup.json`.
fn restore_config(paths: &Paths, files: &Path, stamp: &str) -> Result<()> {
    let staged = files.join("config").join("config.toml");
    if staged.is_file() {
        let live = paths.config_file();
        let text = fs::read_to_string(&staged).unwrap_or_default();
        let (candidate, notes) = localise_config(&text);
        for n in &notes {
            tracing::warn!("restored config.toml: {n}");
        }
        crate::fsutil::create_private_dir(&paths.config).map_err(|e| io_err(&paths.config, e))?;
        match Config::parse(&candidate) {
            Ok(_) => {
                if live.exists() {
                    let to = sibling(&live, &format!("pre-restore-{stamp}"));
                    if !to.exists() {
                        fs::rename(&live, &to).map_err(|e| io_err(&live, e))?;
                    }
                }
                write_private(&live, candidate.as_bytes())?;
            }
            Err(e) => {
                let keep = sibling(&live, &format!("restored-{stamp}"));
                tracing::error!(error = %e, kept = %keep.display(),
                    "the backup's config.toml does not load on this computer; keeping the current config");
                fs::write(&keep, &text).map_err(|e| io_err(&keep, e))?;
            }
        }
        fs::remove_file(&staged).map_err(|e| io_err(&staged, e))?;
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
    Ok(())
}

/// Make a config from another computer usable here: comment out a `[vault] path` that is not
/// an absolute path on this OS (a macOS `/Users/...` on Windows, a `C:\...` elsewhere), so the
/// default vault is used. Returns the text and what was changed.
pub(crate) fn localise_config(text: &str) -> (String, Vec<String>) {
    let Ok(table) = text.parse::<toml::Table>() else {
        return (text.to_string(), Vec::new());
    };
    let path = table
        .get("vault")
        .and_then(|v| v.get("path"))
        .and_then(|p| p.as_str());
    let Some(path) = path else {
        return (text.to_string(), Vec::new());
    };
    let section = crate::config::VaultSection {
        path: Some(path.to_string()),
        ..Default::default()
    };
    if crate::knowledge::vault_root(&section, Path::new("/")).is_ok() {
        return (text.to_string(), Vec::new());
    }
    let mut out = String::with_capacity(text.len() + 80);
    let mut in_vault = false;
    let mut done = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with('[') {
            in_vault = t.trim_end().trim_end_matches(|c: char| c != ']') == "[vault]";
        }
        let is_path = in_vault
            && !done
            && t.strip_prefix("path")
                .is_some_and(|r| r.trim_start().starts_with('='));
        if is_path {
            out.push_str(
                "# Restored from another computer; this vault path does not exist here:\n# ",
            );
            done = true;
        }
        out.push_str(line);
    }
    let note =
        format!("[vault] path {path:?} is not a path on this computer; using the default vault");
    if done {
        (out, vec![note])
    } else {
        (text.to_string(), Vec::new())
    }
}

/// Step 4: the backup's vault into the vault directory of the config now in place, only if
/// that is missing or empty. Copied into a temporary sibling, renamed into place, marked done.
fn restore_vault(paths: &Paths, root: &Path, ready: &Ready) -> Result<()> {
    let done = root.join(VAULT_DONE);
    let from = root.join(STAGED_FILES).join("vault");
    if done.exists() || !from.is_dir() {
        return Ok(());
    }
    let mark = || fs::write(&done, b"").map_err(|e| io_err(&done, e));
    let config = if paths.config_file().exists() {
        Config::load_or_create(paths).map(|(c, _)| c).ok()
    } else {
        Some(Config::default_local())
    };
    let Some(config) = config.filter(|c| c.vault.enabled) else {
        tracing::info!("vault disabled in the restored config; its pages are not restored");
        return mark();
    };
    let default = ready
        .default_vault
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::knowledge::default_vault_dir(paths));
    let vault = crate::knowledge::vault_root(&config.vault, &default)?;
    let empty = match fs::read_dir(&vault) {
        Ok(mut rd) => rd.next().is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(io_err(&vault, e)),
    };
    if !empty {
        tracing::info!(vault = %vault.display(), "vault is not empty; its pages are kept and re-rendered from the restored store");
        return mark();
    }
    let name = vault.file_name().unwrap_or_default().to_string_lossy();
    let tmp = vault.with_file_name(format!(".{name}.nugget-restore-{}", ready.stamp));
    remove_tree(&tmp);
    if let Some(parent) = tmp.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    fs::create_dir(&tmp).map_err(|e| io_err(&tmp, e))?;
    let mut found = Vec::new();
    disk::walk_files(&from, &from, &mut found)?;
    for (rel, path) in found {
        let to = join_rel(&tmp, &rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        fs::copy(&path, &to).map_err(|e| io_err(&to, e))?;
    }
    checkpoint("vault-copied")?;
    if vault.exists() {
        fs::remove_dir(&vault).map_err(|e| io_err(&vault, e))?; // empty, checked above
    }
    fs::rename(&tmp, &vault).map_err(|e| io_err(&vault, e))?;
    if let Some(parent) = vault.parent() {
        disk::sync_dir(parent);
    }
    tracing::info!(vault = %vault.display(), "vault restored");
    mark()
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

/// `root` joined with a validated `/`-separated relative path.
fn join_rel(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, s| p.join(s))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A vault path from the other OS family.
    fn foreign_vault() -> &'static str {
        if cfg!(windows) {
            "/Users/sam/Nugget Vault"
        } else {
            "C:\\\\Users\\\\sam\\\\Nugget Vault"
        }
    }

    #[test]
    fn a_foreign_vault_path_is_commented_out() {
        let text = format!(
            "[runtime]\nenabled = false\n\n[vault]\ndebounce = 150\npath = \"{}\"\n\n[mcp]\nport = 0\n",
            foreign_vault()
        );
        assert!(
            Config::parse(&text).is_ok(),
            "it parses; only the vault root is refused at start"
        );
        let (out, notes) = localise_config(&text);
        assert_eq!(notes.len(), 1, "{out}");
        let cfg = Config::parse(&out).unwrap();
        assert_eq!(cfg.vault.path, None);
        let want = Config::parse("[vault]\ndebounce = 150\n").unwrap();
        assert_eq!(
            cfg.vault.render.debounce, want.vault.render.debounce,
            "the rest of [vault] stays"
        );
        assert!(out.contains("# path = "), "{out}");
        assert!(out.contains("[mcp]\nport = 0"));
    }

    #[test]
    fn a_local_vault_path_is_kept() {
        let here = std::env::temp_dir().join("vault");
        let text = format!("[vault]\npath = {:?}\n", here.display().to_string());
        let (out, notes) = localise_config(&text);
        assert!(notes.is_empty());
        assert_eq!(out, text);
        let (out, notes) = localise_config("[vault]\nenabled = true\n");
        assert!(notes.is_empty() && out == "[vault]\nenabled = true\n");
    }
}
