//! The store's 256-bit database key: OS keychain first, a `0600` key file as fallback.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use hmac::{Hmac, Mac};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Keychain service name (matches the app's `ProjectDirs` qualifier).
const KEYCHAIN_SERVICE: &str = "dev.nugget.Nugget";
/// Key file name under `Paths::data` when no keychain is usable.
const KEY_FILE_NAME: &str = "store.key";
/// Label of [`StoreKey::key_id`]: `HMAC-SHA256(store_key, label)`.
const KEY_ID_LABEL: &[u8] = b"nugget-backup-key-id-v1";
/// Set to `0`, `off`, `false` or `no` to skip the OS keychain (portable installs, headless boxes).
pub const KEYCHAIN_ENV: &str = "NUGGET_KEYCHAIN";

/// Where a [`StoreKey`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// The OS keychain (macOS Keychain, Windows Credential Manager, Secret Service).
    Keychain,
    /// The fallback key file under `Paths::data`.
    File,
    /// Supplied by the caller ([`StoreKey::from_bytes`]).
    Provided,
}

/// A 256-bit SQLCipher key. Zeroed on drop and never printed.
#[derive(Clone)]
pub struct StoreKey {
    bytes: Zeroizing<[u8; 32]>,
    source: KeySource,
}

impl fmt::Debug for StoreKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreKey")
            .field("bytes", &"<redacted>")
            .field("source", &self.source)
            .finish()
    }
}

impl StoreKey {
    /// Use explicit key bytes (tests, or callers managing keys themselves).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            bytes: Zeroizing::new(bytes),
            source: KeySource::Provided,
        }
    }

    /// A fresh random key from the OS CSPRNG.
    pub fn generate() -> Result<Self> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(bytes.as_mut_slice())
            .map_err(|e| Error::Other(format!("OS random number generator failed: {e}")))?;
        Ok(Self {
            bytes,
            source: KeySource::Provided,
        })
    }

    /// Load the store key, creating it on first run.
    ///
    /// Order: the OS keychain entry; else an existing key file under `paths.data`; else a new
    /// random key stored in the keychain (verified by reading it back) or, if no keychain is
    /// usable, in a new `0600` key file (with a warning). Set `NUGGET_KEYCHAIN=off` to skip the
    /// keychain.
    ///
    /// Fails closed: if the database already exists but no key can be found (for example the
    /// keychain is temporarily locked), it returns an error instead of minting a new key that
    /// would make the existing data unreadable.
    pub fn load_or_create(paths: &Paths) -> Result<Self> {
        if keychain_disabled_by_env() {
            tracing::info!("{KEYCHAIN_ENV} disables the OS keychain; using the key file");
            return load_or_create_with(paths, None);
        }
        let keychain = os::keychain_for(&paths.data);
        load_or_create_with(paths, keychain.as_ref().map(|k| k as &dyn SecretBackend))
    }

    /// Where this key came from.
    pub fn source(&self) -> KeySource {
        self.source
    }

    /// A public identifier of this key: the first 16 hex characters of
    /// `HMAC-SHA256(key, "nugget-backup-key-id-v1")`. Recorded in backup manifests and key
    /// envelopes so a restore can tell whether a stored key opens a backup without asking for
    /// its passphrase. Reveals nothing about the key.
    pub fn key_id(&self) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.bytes.as_slice())
            .expect("HMAC accepts any key length");
        mac.update(KEY_ID_LABEL);
        hex::encode(&mac.finalize().into_bytes()[..8])
    }

    /// Load the key of an existing home without ever creating one: the OS keychain entry (unless
    /// [`KEYCHAIN_ENV`] disables it), else the key file under `paths.data`. `Ok(None)` when
    /// neither holds a key (a keychain that cannot be reached counts as holding none, with a
    /// warning).
    pub fn load_existing(paths: &Paths) -> Result<Option<StoreKey>> {
        let keychain = (!keychain_disabled_by_env())
            .then(|| os::keychain_for(&paths.data))
            .flatten();
        load_existing_with(paths, keychain.as_ref().map(|k| k as &dyn SecretBackend))
    }

    /// Make `key` the store key of `paths`, as [`StoreKey::load_or_create`] will find it: in the
    /// OS keychain when one is usable (and not disabled through [`KEYCHAIN_ENV`]), else in a
    /// `0600` key file under `paths.data`. Returns where it was stored.
    ///
    /// A different key already stored there (keychain or key file) is first saved to
    /// `<data>/store.key.replaced-<UTC timestamp>` (`0600`), so a database it encrypts (for
    /// example the one moved aside by a restore) stays readable; it is never silently discarded.
    /// A key file left over once the key is in the keychain is removed after that copy.
    ///
    /// A malformed secret found there is saved the same way (as found) and replaced.
    ///
    /// Refuses (`Error::Storage`) rather than write a key file that a keychain entry would
    /// shadow on the next start: when the keychain is enabled but cannot be read and the home
    /// has no key file yet (the current key may be in a locked keychain: unlock it, or set
    /// [`KEYCHAIN_ENV`]`=off` consistently), and when the keychain holds a different key but
    /// refuses the new one or does not return it after storing it (the previous entry is then
    /// put back, best effort). When the keychain cannot be read but the home already keeps its
    /// key in a file, the file is used, as [`StoreKey::load_or_create`] does.
    pub fn install(paths: &Paths, key: &StoreKey) -> Result<KeySource> {
        if keychain_disabled_by_env() {
            return install_with(paths, key, None);
        }
        let keychain = os::keychain_for(&paths.data);
        install_with(
            paths,
            key,
            keychain.as_ref().map(|k| k as &dyn SecretBackend),
        )
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    fn to_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(hex::encode(self.bytes.as_slice()))
    }

    fn from_hex(s: &str, source: KeySource) -> Result<Self> {
        let s = s.trim();
        let mut bytes = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(s, bytes.as_mut_slice()).map_err(|_| {
            Error::Integrity(format!(
                "store key from {source:?} is malformed (expected 64 hex characters)"
            ))
        })?;
        Ok(Self { bytes, source })
    }
}

fn keychain_disabled_by_env() -> bool {
    std::env::var(KEYCHAIN_ENV)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            )
        })
        .unwrap_or(false)
}

/// Minimal secret storage interface, so the selection logic is testable without a real keychain.
pub(crate) trait SecretBackend {
    /// `Ok(None)` when the backend works but has no entry; `Err` when it is unusable.
    fn get(&self) -> std::result::Result<Option<Zeroizing<String>>, String>;
    fn set(&self, secret: &str) -> std::result::Result<(), String>;
}

/// The OS keychain entry `account` under this app's service, for secrets other than the store
/// key (e.g. the audit chain anchor). `None` when the keychain is disabled through
/// [`KEYCHAIN_ENV`] or this target has no supported keychain.
pub(crate) fn keychain_entry(account: String) -> Option<Box<dyn SecretBackend + Send + Sync>> {
    if keychain_disabled_by_env() {
        return None;
    }
    os::keychain_account_entry(account).map(|k| Box::new(k) as Box<dyn SecretBackend + Send + Sync>)
}

pub(crate) fn key_file(paths: &Paths) -> PathBuf {
    paths.data.join(KEY_FILE_NAME)
}

/// Keychain account for a data directory, so separate `NUGGET_HOME`s get separate keys.
fn keychain_account(data_dir: &Path) -> String {
    let abs = std::path::absolute(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let digest = Sha256::digest(abs.to_string_lossy().as_bytes());
    format!("store-key-{}", &hex::encode(digest)[..16])
}

pub(crate) fn load_or_create_with(
    paths: &Paths,
    keychain: Option<&dyn SecretBackend>,
) -> Result<StoreKey> {
    let keychain_error = match keychain.map(|k| k.get()) {
        Some(Ok(Some(hex))) => return StoreKey::from_hex(&hex, KeySource::Keychain),
        Some(Ok(None)) => None,
        Some(Err(e)) => Some(e),
        None => Some("disabled".to_owned()),
    };

    let file = key_file(paths);
    if file.exists() {
        return read_key_file(&file);
    }

    let db = paths.db_file();
    if db.exists() {
        return Err(Error::Storage(format!(
            "the store at {} exists but its key was not found (keychain: {}; no key file at {}); \
             refusing to create a new key that would make it unreadable",
            db.display(),
            keychain_error.as_deref().unwrap_or("no entry"),
            file.display()
        )));
    }

    let key = StoreKey::generate()?;
    if let (Some(kc), None) = (keychain, keychain_error.as_ref()) {
        let hex = key.to_hex();
        let stored = kc.set(&hex).and_then(|()| match kc.get() {
            Ok(Some(back)) if *back == *hex => Ok(()),
            Ok(_) => Err("keychain did not return the stored key".to_owned()),
            Err(e) => Err(e),
        });
        match stored {
            Ok(()) => {
                tracing::info!("created store key in the OS keychain");
                return Ok(StoreKey {
                    source: KeySource::Keychain,
                    ..key
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "OS keychain rejected the store key; using a key file");
            }
        }
    } else if let Some(e) = &keychain_error {
        tracing::warn!(
            error = %e,
            path = %file.display(),
            "OS keychain unavailable; storing the store key in a 0600 key file"
        );
    }

    write_key_file(&file, &key.to_hex())?;
    Ok(StoreKey {
        source: KeySource::File,
        ..key
    })
}

pub(crate) fn load_existing_with(
    paths: &Paths,
    keychain: Option<&dyn SecretBackend>,
) -> Result<Option<StoreKey>> {
    match keychain.map(|k| k.get()) {
        Some(Ok(Some(hex))) => return StoreKey::from_hex(&hex, KeySource::Keychain).map(Some),
        Some(Err(e)) => {
            tracing::warn!(error = %e, "cannot read the OS keychain; looking for a key file");
        }
        Some(Ok(None)) | None => {}
    }
    let file = key_file(paths);
    if file.exists() {
        return read_key_file(&file).map(Some);
    }
    Ok(None)
}

/// A secret found where a store key is kept: a well-formed key, or the raw text of a malformed
/// one (still saved before it is replaced: it may be a damaged copy worth recovering).
enum Stored {
    Key(StoreKey),
    Malformed(Zeroizing<String>),
}

impl Stored {
    fn parse(raw: Zeroizing<String>, source: KeySource) -> Self {
        match StoreKey::from_hex(&raw, source) {
            Ok(k) => Self::Key(k),
            Err(_) => Self::Malformed(raw),
        }
    }

    fn is(&self, key: &StoreKey) -> bool {
        matches!(self, Self::Key(k) if k.as_bytes() == key.as_bytes())
    }

    /// The text to save: the key's hex, or the malformed text as found.
    fn raw(&self) -> Zeroizing<String> {
        match self {
            Self::Key(k) => k.to_hex(),
            Self::Malformed(raw) => Zeroizing::new(raw.trim().to_owned()),
        }
    }
}

pub(crate) fn install_with(
    paths: &Paths,
    key: &StoreKey,
    keychain: Option<&dyn SecretBackend>,
) -> Result<KeySource> {
    let new_hex = key.to_hex();
    let file = key_file(paths);
    let in_file = if file.exists() {
        tighten_permissions(&file)?;
        Some(Stored::parse(
            Zeroizing::new(fs::read_to_string(&file)?),
            KeySource::File,
        ))
    } else {
        None
    };
    // What the keychain holds: Ok(Some/None) when it answers, Err when it is unusable.
    let in_keychain = match keychain.map(|k| k.get()) {
        Some(Ok(v)) => Ok(v.map(|raw| Stored::parse(raw, KeySource::Keychain))),
        Some(Err(e)) => {
            if in_file.is_none() {
                // This home's current key may well be in there (a locked keychain): a key file
                // written now would be shadowed by it on the next start.
                return Err(Error::Storage(format!(
                    "cannot read the OS keychain ({e}), which may hold this home's current store \
                     key; a key file written now would be shadowed by it. Unlock the keychain and \
                     retry, or set {KEYCHAIN_ENV}=off (for this and every later start) to keep the \
                     key in a file"
                )));
            }
            // The home already keeps its key in a file and the keychain does not answer: stay
            // with the file, as `load_or_create` does.
            Err(e)
        }
        None => Err("disabled".to_owned()),
    };

    // Save every different secret first, so nothing below can lose one.
    let mut saved: Vec<Zeroizing<String>> = Vec::new();
    let mut saved_to: Vec<PathBuf> = Vec::new();
    for old in in_keychain
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .into_iter()
        .chain(&in_file)
    {
        let raw = old.raw();
        if !old.is(key) && !saved.contains(&raw) {
            let to = save_replaced_key(paths, &raw)?;
            match old {
                Stored::Key(k) => tracing::warn!(
                    path = %to.display(),
                    key_id = %k.key_id(),
                    "store key replaced; the previous key was saved"
                ),
                Stored::Malformed(_) => tracing::warn!(
                    path = %to.display(),
                    "a malformed store key was replaced; its text was saved"
                ),
            }
            saved.push(raw);
            saved_to.push(to);
        }
    }
    if !saved_to.is_empty() {
        // The copies are durable before anything they protect is overwritten or removed.
        sync_dir(&paths.data);
    }

    if let (Some(kc), Ok(current)) = (keychain, &in_keychain) {
        if !current.as_ref().is_some_and(|c| c.is(key)) {
            if let Err(e) = kc.set(&new_hex) {
                if current.is_some() {
                    return Err(Error::Storage(format!(
                        "the OS keychain holds another store key and refused the new one ({e}); \
                         a key file would be shadowed by it"
                    )));
                }
                tracing::warn!(error = %e, "OS keychain rejected the store key; using a key file");
                return install_file(paths, key, &new_hex, in_file.as_ref());
            }
            match kc.get() {
                Ok(Some(back)) if *back == *new_hex => {}
                // Accepted but not kept (like keyring's mock store) and nothing to shadow.
                Ok(None) if current.is_none() => {
                    tracing::warn!("OS keychain did not keep the store key; using a key file");
                    return install_file(paths, key, &new_hex, in_file.as_ref());
                }
                readback => {
                    let what = match readback {
                        Ok(Some(_)) => "returned another value".to_owned(),
                        Ok(None) => "returned nothing".to_owned(),
                        Err(e) => format!("could not be read back: {e}"),
                    };
                    let restored = match current {
                        Some(old) => match kc.set(&old.raw()) {
                            Ok(()) => "the previous entry was put back".to_owned(),
                            Err(e) => format!("putting the previous entry back failed too: {e}"),
                        },
                        None => "there was no previous entry to put back".to_owned(),
                    };
                    let copies = if saved_to.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; previous keys were saved to {}",
                            saved_to
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    return Err(Error::Storage(format!(
                        "the OS keychain accepted the new store key but then {what}; {restored}{copies}"
                    )));
                }
            }
        }
        if in_file.is_some() {
            // Its secret (if different) was saved above; left in place it would only confuse a
            // later fallback.
            fs::remove_file(&file)?;
            sync_dir(&paths.data);
        }
        return Ok(KeySource::Keychain);
    }
    if let Err(e) = &in_keychain {
        tracing::warn!(
            error = %e,
            path = %file.display(),
            "OS keychain unavailable; storing the store key in a 0600 key file"
        );
    }
    install_file(paths, key, &new_hex, in_file.as_ref())
}

/// Make `key` the content of the key file (unless it already is).
fn install_file(
    paths: &Paths,
    key: &StoreKey,
    new_hex: &str,
    in_file: Option<&Stored>,
) -> Result<KeySource> {
    if in_file.is_some_and(|f| f.is(key)) {
        return Ok(KeySource::File);
    }
    // Written beside the key file, then renamed over it: a crash leaves the old or the new key,
    // never a torn file.
    let file = key_file(paths);
    let tmp = paths
        .data
        .join(format!("{KEY_FILE_NAME}.tmp-{}", random_suffix()?));
    write_key_file(&tmp, new_hex)?;
    if let Err(e) = fs::rename(&tmp, &file) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    sync_dir(&paths.data);
    Ok(KeySource::File)
}

/// Save `raw` (a key's hex, or a malformed key's text) to a new
/// `<data>/store.key.replaced-<UTC timestamp>[-n]` file (`0600`).
fn save_replaced_key(paths: &Paths, raw: &str) -> Result<PathBuf> {
    let now = time::OffsetDateTime::now_utc();
    let stamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    for n in 0..1000 {
        let name = match n {
            0 => format!("{KEY_FILE_NAME}.replaced-{stamp}"),
            n => format!("{KEY_FILE_NAME}.replaced-{stamp}-{n}"),
        };
        let path = paths.data.join(name);
        match write_key_file(&path, raw) {
            Ok(()) => return Ok(path),
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::Storage(
        "cannot find a free name to save the replaced store key".into(),
    ))
}

fn random_suffix() -> Result<String> {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b)
        .map_err(|e| Error::Other(format!("OS random number generator failed: {e}")))?;
    Ok(hex::encode(b))
}

/// Flush `path`'s content to disk. Opened for writing: Windows' `FlushFileBuffers` refuses a
/// read-only handle.
pub(crate) fn sync_file(path: &Path) -> std::io::Result<()> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()
}

/// Make a rename in `dir` durable (Unix; best effort). Windows cannot open a directory this
/// way, and its file system commits renames itself.
pub(crate) fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Err(e) = fs::File::open(dir).and_then(|d| d.sync_all()) {
        tracing::debug!(path = %dir.display(), error = %e, "cannot sync directory");
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn read_key_file(path: &Path) -> Result<StoreKey> {
    tighten_permissions(path)?;
    let contents = Zeroizing::new(fs::read_to_string(path)?);
    StoreKey::from_hex(&contents, KeySource::File)
}

/// Create the key file exclusively (`O_EXCL`), owner-only (`0600` on Unix, a protected DACL on
/// Windows). A crash mid-write leaves a malformed file, which is reported rather than silently
/// replaced.
fn write_key_file(path: &Path, hex: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    // Before the key is written, so it is never readable through an inherited ACL.
    crate::acl::restrict_file(path);
    let written = f
        .write_all(hex.as_bytes())
        .and_then(|()| f.write_all(b"\n"))
        .and_then(|()| f.sync_all());
    if let Err(e) = written {
        drop(f);
        let _ = fs::remove_file(path);
        return Err(e.into());
    }
    Ok(())
}

/// Create `dir` (and parents). A directory we create is owner-only: `0700` on Unix, a protected
/// DACL (current user and `SYSTEM`) on Windows.
pub(crate) fn create_private_dir(dir: &Path) -> Result<()> {
    if dir.as_os_str().is_empty() || dir.exists() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    crate::acl::restrict_dir(dir);
    Ok(())
}

/// On Unix, warn about and fix a key file readable by group or others. Elsewhere a no-op: the
/// file lives in the per-user profile, whose default ACL is owner-only.
#[cfg(unix)]
fn tighten_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{:o}", mode & 0o777),
            "store key file was accessible to other users; restricting it to 0600"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn tighten_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd"
))]
mod os {
    use super::{KEYCHAIN_SERVICE, SecretBackend, keychain_account};
    use std::path::Path;
    use zeroize::Zeroizing;

    pub(super) struct OsKeychain {
        account: String,
    }

    pub(super) fn keychain_for(data_dir: &Path) -> Option<OsKeychain> {
        keychain_account_entry(keychain_account(data_dir))
    }

    pub(super) fn keychain_account_entry(account: String) -> Option<OsKeychain> {
        Some(OsKeychain { account })
    }

    impl OsKeychain {
        fn entry(&self) -> Result<keyring::Entry, String> {
            keyring::Entry::new(KEYCHAIN_SERVICE, &self.account).map_err(|e| e.to_string())
        }
    }

    impl SecretBackend for OsKeychain {
        fn get(&self) -> Result<Option<Zeroizing<String>>, String> {
            match self.entry()?.get_password() {
                Ok(secret) => Ok(Some(Zeroizing::new(secret))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        }

        fn set(&self, secret: &str) -> Result<(), String> {
            self.entry()?
                .set_password(secret)
                .map_err(|e| e.to_string())
        }
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd"
)))]
mod os {
    use super::SecretBackend;
    use std::path::Path;
    use zeroize::Zeroizing;

    /// No supported keychain on this target; always falls back to the key file.
    pub(super) enum OsKeychain {}

    pub(super) fn keychain_for(_data_dir: &Path) -> Option<OsKeychain> {
        None
    }

    pub(super) fn keychain_account_entry(_account: String) -> Option<OsKeychain> {
        None
    }

    impl SecretBackend for OsKeychain {
        fn get(&self) -> Result<Option<Zeroizing<String>>, String> {
            match *self {}
        }
        fn set(&self, _secret: &str) -> Result<(), String> {
            match *self {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeKeychain(Mutex<Option<String>>);

    impl SecretBackend for FakeKeychain {
        fn get(&self) -> std::result::Result<Option<Zeroizing<String>>, String> {
            Ok(self.0.lock().unwrap().clone().map(Zeroizing::new))
        }
        fn set(&self, secret: &str) -> std::result::Result<(), String> {
            *self.0.lock().unwrap() = Some(secret.to_owned());
            Ok(())
        }
    }

    struct BrokenKeychain;

    impl SecretBackend for BrokenKeychain {
        fn get(&self) -> std::result::Result<Option<Zeroizing<String>>, String> {
            Err("no secret service on this bus".into())
        }
        fn set(&self, _: &str) -> std::result::Result<(), String> {
            Err("no secret service on this bus".into())
        }
    }

    /// Accepts writes but forgets them (like keyring's mock store).
    struct ForgetfulKeychain;

    impl SecretBackend for ForgetfulKeychain {
        fn get(&self) -> std::result::Result<Option<Zeroizing<String>>, String> {
            Ok(None)
        }
        fn set(&self, _: &str) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path().to_path_buf());
        (dir, paths)
    }

    #[test]
    fn prefers_keychain_and_reuses_the_key() {
        let (_d, paths) = paths();
        let kc = FakeKeychain::default();
        let k1 = load_or_create_with(&paths, Some(&kc)).unwrap();
        assert_eq!(k1.source(), KeySource::Keychain);
        assert!(
            !key_file(&paths).exists(),
            "no key file when keychain works"
        );
        let k2 = load_or_create_with(&paths, Some(&kc)).unwrap();
        assert_eq!(k1.as_bytes(), k2.as_bytes());
    }

    #[test]
    fn falls_back_to_private_key_file() {
        let (_d, paths) = paths();
        for kc in [Some(&BrokenKeychain as &dyn SecretBackend), None] {
            let _ = fs::remove_file(key_file(&paths));
            let k1 = load_or_create_with(&paths, kc).unwrap();
            assert_eq!(k1.source(), KeySource::File);
            let k2 = load_or_create_with(&paths, kc).unwrap();
            assert_eq!(k1.as_bytes(), k2.as_bytes());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(key_file(&paths)).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600);
            }
        }
    }

    #[test]
    fn keychain_that_does_not_persist_falls_back_to_file() {
        let (_d, paths) = paths();
        let k = load_or_create_with(&paths, Some(&ForgetfulKeychain)).unwrap();
        assert_eq!(k.source(), KeySource::File);
        assert!(key_file(&paths).exists());
    }

    #[test]
    fn existing_key_file_wins_over_empty_keychain() {
        let (_d, paths) = paths();
        let k1 = load_or_create_with(&paths, None).unwrap();
        let kc = FakeKeychain::default();
        let k2 = load_or_create_with(&paths, Some(&kc)).unwrap();
        assert_eq!(k1.as_bytes(), k2.as_bytes());
        assert!(kc.0.lock().unwrap().is_none(), "must not mint a second key");
    }

    #[test]
    fn refuses_new_key_for_existing_database() {
        let (_d, paths) = paths();
        fs::create_dir_all(&paths.data).unwrap();
        fs::write(paths.db_file(), b"not empty").unwrap();
        let err = load_or_create_with(&paths, Some(&BrokenKeychain)).unwrap_err();
        assert!(matches!(err, Error::Storage(_)), "{err}");
        assert!(!key_file(&paths).exists());
    }

    #[test]
    fn malformed_key_file_is_an_integrity_error() {
        let (_d, paths) = paths();
        fs::create_dir_all(&paths.data).unwrap();
        fs::write(key_file(&paths), "abc").unwrap();
        let err = load_or_create_with(&paths, None).unwrap_err();
        assert!(matches!(err, Error::Integrity(_)), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn loose_key_file_permissions_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, paths) = paths();
        let k1 = load_or_create_with(&paths, None).unwrap();
        fs::set_permissions(key_file(&paths), fs::Permissions::from_mode(0o644)).unwrap();
        let k2 = load_or_create_with(&paths, None).unwrap();
        assert_eq!(k1.as_bytes(), k2.as_bytes());
        let mode = fs::metadata(key_file(&paths)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn keychain_account_depends_on_data_dir() {
        assert_ne!(
            keychain_account(Path::new("/a/data")),
            keychain_account(Path::new("/b/data"))
        );
    }

    #[test]
    fn debug_does_not_leak_key() {
        let k = StoreKey::from_bytes([0xAB; 32]);
        let s = format!("{k:?}");
        assert!(!s.to_lowercase().contains("abab"), "{s}");
    }

    #[test]
    fn generated_keys_differ() {
        let a = StoreKey::generate().unwrap();
        let b = StoreKey::generate().unwrap();
        assert_ne!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn key_id_is_stable_short_and_key_specific() {
        let a = StoreKey::from_bytes([1; 32]);
        assert_eq!(a.key_id(), StoreKey::from_bytes([1; 32]).key_id());
        assert_eq!(a.key_id().len(), 16);
        assert!(a.key_id().chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.key_id(), StoreKey::from_bytes([2; 32]).key_id());
        assert!(!hex::encode(a.as_bytes()).contains(&a.key_id()));
    }

    /// The `store.key.replaced-*` files under the data directory, with their keys.
    fn replaced(paths: &Paths) -> Vec<[u8; 32]> {
        let Ok(dir) = fs::read_dir(&paths.data) else {
            return Vec::new();
        };
        let mut out: Vec<[u8; 32]> = dir
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("store.key.replaced-")
            })
            .map(|p| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = fs::metadata(&p).unwrap().permissions().mode();
                    assert_eq!(mode & 0o777, 0o600, "{}", p.display());
                }
                *read_key_file(&p).unwrap().as_bytes()
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn load_existing_never_creates_a_key() {
        let (_d, paths) = paths();
        let kc = FakeKeychain::default();
        assert!(load_existing_with(&paths, Some(&kc)).unwrap().is_none());
        assert!(
            load_existing_with(&paths, Some(&BrokenKeychain))
                .unwrap()
                .is_none()
        );
        assert!(load_existing_with(&paths, None).unwrap().is_none());
        assert!(kc.0.lock().unwrap().is_none());
        assert!(!key_file(&paths).exists());

        let k = load_or_create_with(&paths, Some(&kc)).unwrap();
        let back = load_existing_with(&paths, Some(&kc)).unwrap().unwrap();
        assert_eq!(
            (back.as_bytes(), back.source()),
            (k.as_bytes(), KeySource::Keychain)
        );

        let (_d2, paths) = self::paths();
        let k = load_or_create_with(&paths, None).unwrap();
        for kc in [None, Some(&BrokenKeychain as &dyn SecretBackend)] {
            let back = load_existing_with(&paths, kc).unwrap().unwrap();
            assert_eq!(
                (back.as_bytes(), back.source()),
                (k.as_bytes(), KeySource::File)
            );
        }
    }

    #[test]
    fn install_into_keychain_saves_the_replaced_key() {
        let (_d, paths) = paths();
        let kc = FakeKeychain::default();
        // Fresh home: straight into the keychain, nothing to save.
        let first = StoreKey::from_bytes([1; 32]);
        assert_eq!(
            install_with(&paths, &first, Some(&kc)).unwrap(),
            KeySource::Keychain
        );
        assert!(replaced(&paths).is_empty());
        // Installing the same key again changes nothing.
        assert_eq!(
            install_with(&paths, &first, Some(&kc)).unwrap(),
            KeySource::Keychain
        );
        assert!(replaced(&paths).is_empty());

        let second = StoreKey::from_bytes([2; 32]);
        assert_eq!(
            install_with(&paths, &second, Some(&kc)).unwrap(),
            KeySource::Keychain
        );
        assert_eq!(replaced(&paths), vec![[1; 32]]);
        let loaded = load_or_create_with(&paths, Some(&kc)).unwrap();
        assert_eq!(loaded.as_bytes(), second.as_bytes());

        // A key file left from before the keychain worked is saved and removed.
        fs::write(key_file(&paths), hex::encode([3u8; 32])).unwrap();
        let third = StoreKey::from_bytes([4; 32]);
        install_with(&paths, &third, Some(&kc)).unwrap();
        assert!(!key_file(&paths).exists());
        assert_eq!(replaced(&paths), vec![[1; 32], [2; 32], [3; 32]]);
        assert_eq!(
            load_existing_with(&paths, Some(&kc))
                .unwrap()
                .unwrap()
                .as_bytes(),
            third.as_bytes()
        );
    }

    #[test]
    fn install_into_key_file_without_a_keychain() {
        for kc in [None, Some(&ForgetfulKeychain as &dyn SecretBackend)] {
            let (_d, paths) = paths();
            let first = StoreKey::from_bytes([5; 32]);
            assert_eq!(install_with(&paths, &first, kc).unwrap(), KeySource::File);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(key_file(&paths)).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600);
            }
            assert_eq!(
                load_or_create_with(&paths, kc).unwrap().as_bytes(),
                first.as_bytes()
            );
            assert_eq!(install_with(&paths, &first, kc).unwrap(), KeySource::File);
            assert!(replaced(&paths).is_empty());

            let second = StoreKey::from_bytes([6; 32]);
            install_with(&paths, &second, kc).unwrap();
            assert_eq!(replaced(&paths), vec![[5; 32]]);
            assert_eq!(
                load_or_create_with(&paths, kc).unwrap().as_bytes(),
                second.as_bytes()
            );
            // No temporary files are left behind.
            let names: Vec<String> = fs::read_dir(&paths.data)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert!(names.iter().all(|n| !n.contains(".tmp-")), "{names:?}");
        }
    }

    /// A keychain that holds a key but refuses writes.
    struct ReadOnlyKeychain(String);

    impl SecretBackend for ReadOnlyKeychain {
        fn get(&self) -> std::result::Result<Option<Zeroizing<String>>, String> {
            Ok(Some(Zeroizing::new(self.0.clone())))
        }
        fn set(&self, _: &str) -> std::result::Result<(), String> {
            Err("read-only".into())
        }
    }

    #[test]
    fn install_refuses_to_shadow_a_keychain_key_it_cannot_replace() {
        let (_d, paths) = paths();
        let kc = ReadOnlyKeychain(hex::encode([7u8; 32]));
        let err = install_with(&paths, &StoreKey::from_bytes([8; 32]), Some(&kc)).unwrap_err();
        assert!(matches!(err, Error::Storage(_)), "{err}");
        assert!(!key_file(&paths).exists());
        // The old key is saved all the same (nothing is ever lost).
        assert_eq!(replaced(&paths), vec![[7; 32]]);
        // Installing the key it already holds is fine.
        assert_eq!(
            install_with(&paths, &StoreKey::from_bytes([7; 32]), Some(&kc)).unwrap(),
            KeySource::Keychain
        );
    }

    /// An enabled keychain that cannot be read may hold the current key: no key file is
    /// written beside it, unless the home already keeps its key in a file.
    #[test]
    fn install_refuses_an_unreadable_keychain_without_a_key_file() {
        let (_d, paths) = paths();
        let err = install_with(
            &paths,
            &StoreKey::from_bytes([1; 32]),
            Some(&BrokenKeychain),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Storage(m) if m.contains(KEYCHAIN_ENV)),
            "{err}"
        );
        assert!(!key_file(&paths).exists());

        load_or_create_with(&paths, None).unwrap();
        let second = StoreKey::from_bytes([2; 32]);
        assert_eq!(
            install_with(&paths, &second, Some(&BrokenKeychain)).unwrap(),
            KeySource::File
        );
        assert_eq!(replaced(&paths).len(), 1);
        assert_eq!(
            load_or_create_with(&paths, Some(&BrokenKeychain))
                .unwrap()
                .as_bytes(),
            second.as_bytes()
        );
    }

    /// Accepts writes, but reads back something else.
    struct LyingKeychain(Mutex<Vec<String>>);

    impl SecretBackend for LyingKeychain {
        fn get(&self) -> std::result::Result<Option<Zeroizing<String>>, String> {
            let sets = self.0.lock().unwrap();
            // Always the first value ever stored (the "old" key), whatever was set since.
            Ok(sets.first().cloned().map(Zeroizing::new))
        }
        fn set(&self, secret: &str) -> std::result::Result<(), String> {
            self.0.lock().unwrap().push(secret.to_owned());
            Ok(())
        }
    }

    #[test]
    fn install_puts_the_previous_keychain_entry_back_when_readback_fails() {
        let (_d, paths) = paths();
        let old = hex::encode([3u8; 32]);
        let kc = LyingKeychain(Mutex::new(vec![old.clone()]));
        let err = install_with(&paths, &StoreKey::from_bytes([4; 32]), Some(&kc)).unwrap_err();
        assert!(
            matches!(&err, Error::Storage(m) if m.contains("put back") && m.contains("replaced-")),
            "{err}"
        );
        let sets = kc.0.lock().unwrap().clone();
        assert_eq!(
            sets.last(),
            Some(&old),
            "previous entry not restored: {sets:?}"
        );
        assert_eq!(replaced(&paths), vec![[3; 32]]);
        assert!(!key_file(&paths).exists());
    }

    #[test]
    fn install_replaces_a_malformed_key_and_keeps_its_text() {
        let (_d, paths) = paths();
        fs::create_dir_all(&paths.data).unwrap();
        fs::write(key_file(&paths), "not a key\n").unwrap();
        let key = StoreKey::from_bytes([5; 32]);
        assert_eq!(install_with(&paths, &key, None).unwrap(), KeySource::File);
        assert_eq!(
            load_or_create_with(&paths, None).unwrap().as_bytes(),
            key.as_bytes()
        );
        let saved: Vec<String> = fs::read_dir(&paths.data)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(".replaced-"))
            .map(|p| fs::read_to_string(p).unwrap())
            .collect();
        assert_eq!(saved, vec!["not a key\n".to_owned()]);

        // A malformed keychain entry too.
        let (_d, paths) = self::paths();
        let kc = FakeKeychain(Mutex::new(Some("garbage".into())));
        assert_eq!(
            install_with(&paths, &key, Some(&kc)).unwrap(),
            KeySource::Keychain
        );
        assert_eq!(
            load_or_create_with(&paths, Some(&kc)).unwrap().as_bytes(),
            key.as_bytes()
        );
    }

    #[test]
    fn sync_file_opens_for_writing() {
        let (_d, paths) = paths();
        fs::create_dir_all(&paths.data).unwrap();
        let f = paths.data.join("f");
        fs::write(&f, b"x").unwrap();
        sync_file(&f).unwrap();
        sync_dir(&paths.data);
        assert_eq!(fs::read(&f).unwrap(), b"x");
    }
}
