//! Installed plugins: a content-addressed directory under the data dir.
//!
//! ```text
//! <data>/plugins/
//!   blobs/sha256/<hex>        manifests and components, by content hash
//!   index.json                id -> installed version, hashes, signature, approved capabilities,
//!                             enabled flag, approval seal
//!   approval.key              32 random bytes (0600) keying the approval seals
//!   state/<id>/               the plugin's key-value store (see `kv`)
//! ```
//!
//! Install and update verify the package signature against trusted keys, then ask the user to
//! approve the declared capabilities. An update must raise the version (no downgrades), must be
//! signed by the same key as the installed version, and asks again only when it requests more
//! than was approved before. [`PluginStore::preview`] runs the same checks without writing, so a
//! caller can show the user what an install or update would do before asking. Each plugin has an
//! `enabled` flag (true on install, kept across updates); the store only records it, callers
//! decide not to load a disabled plugin.
//!
//! [`PluginStore::load_trusted`] re-hashes the blobs and re-checks the signature against the
//! caller's *trusted keys* (not the signer written in the index), so a component altered on disk,
//! or a plugin whose signing key is no longer trusted, is refused. Each record also carries an
//! approval seal: HMAC-SHA256, keyed with `approval.key`, over the plugin id, both blob hashes, the
//! signer and the approved capabilities. Loading recomputes it, so editing the signer or widening
//! the approved capabilities in `index.json` makes the plugin unloadable. This guards against edits
//! to the index only, not against an attacker who can also read the key (anyone who can rewrite
//! the data dir as the same user can). Records written before seals existed fail closed: the user
//! removes and installs the plugin again to re-approve it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use ed25519_dalek::VerifyingKey;
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::manifest::{Capabilities, Manifest};
use crate::package::{
    PluginPackage, VerifiedPackage, decode_signature, parse_verifying_key, sha256_hex,
    verify_signature,
};

/// What the user is asked to approve.
#[derive(Debug)]
pub struct ApprovalRequest<'a> {
    pub manifest: &'a Manifest,
    /// The key that signed the package.
    pub signer: &'a VerifyingKey,
    /// What the user approved for the installed version, on an update.
    pub previously_approved: Option<&'a Capabilities>,
}

/// One installed plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledPlugin {
    pub manifest: Manifest,
    /// The capabilities the user approved. The host enforces these.
    pub approved: Capabilities,
    pub wasm_sha256: String,
    pub manifest_sha256: String,
    /// Hex ed25519 public key that signed the package.
    pub signer: String,
    pub installed_at: OffsetDateTime,
    /// Whether the user wants the plugin loaded.
    pub enabled: bool,
}

/// What installing or updating a package would do, without doing it.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallPreview {
    pub manifest: Manifest,
    /// The trusted key that signed the package.
    pub signer: VerifyingKey,
    /// The installed version, when this would be an update.
    pub installed_version: Option<semver::Version>,
    /// What the user approved for the installed version, when this would be an update.
    pub previously_approved: Option<Capabilities>,
    /// Whether the user must approve: a new install, or an update requesting capabilities beyond
    /// those approved before.
    pub needs_approval: bool,
}

/// An installed plugin with its component bytes, re-verified on disk.
#[derive(Debug, Clone)]
pub struct VerifiedPlugin {
    pub installed: InstalledPlugin,
    pub wasm: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    version: String,
    manifest_sha256: String,
    wasm_sha256: String,
    signature: String,
    signer: String,
    approved: Capabilities,
    #[serde(with = "time::serde::rfc3339")]
    installed_at: OffsetDateTime,
    /// HMAC-SHA256 (hex) binding the approval to the package; see [`approval_seal`]. Absent in
    /// records written before seals existed: such a plugin must be re-approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approval_seal: Option<String>,
    /// Absent in indexes written before the flag existed: those plugins are enabled.
    #[serde(default = "enabled_default")]
    enabled: bool,
}

fn enabled_default() -> bool {
    true
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    plugins: BTreeMap<String, Record>,
}

/// The installed-plugin directory.
#[derive(Debug)]
pub struct PluginStore {
    root: PathBuf,
    approval_key: [u8; 32],
    lock: Mutex<()>,
}

/// File under the store root holding the key of the approval seals.
const APPROVAL_KEY_FILE: &str = "approval.key";

impl PluginStore {
    /// The store at `<data>/plugins`.
    pub fn open(paths: &Paths) -> Result<Self> {
        Self::open_at(paths.data.join("plugins"))
    }

    /// The store at `root`, with the approval key in `<root>/approval.key` (created with fresh
    /// random bytes, mode 0600 on unix, when missing).
    pub fn open_at(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        let key = load_or_create_key(&root.join(APPROVAL_KEY_FILE))?;
        Self::open_with_key(root, key)
    }

    /// The store at `root`, with an approval key supplied by the caller (e.g. kept in the OS
    /// keychain) instead of `<root>/approval.key`.
    pub fn open_with_key(root: PathBuf, approval_key: [u8; 32]) -> Result<Self> {
        std::fs::create_dir_all(root.join("blobs").join("sha256"))?;
        std::fs::create_dir_all(root.join("state"))?;
        Ok(Self {
            root,
            approval_key,
            lock: Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding every plugin's key-value state (pass to `HostConfig::state_dir`).
    pub fn state_root(&self) -> PathBuf {
        self.root.join("state")
    }

    /// Install a plugin that is not installed yet.
    pub fn install(
        &self,
        package: &PluginPackage,
        trusted: &[VerifyingKey],
        approve: &mut dyn FnMut(&ApprovalRequest<'_>) -> bool,
    ) -> Result<InstalledPlugin> {
        let pkg = package.verify(trusted)?;
        let _g = self.guard();
        let mut index = self.read_index()?;
        if index.plugins.contains_key(&pkg.manifest.id) {
            return Err(Error::Invalid(format!(
                "plugin `{}` is already installed; update it instead",
                pkg.manifest.id
            )));
        }
        let req = ApprovalRequest {
            manifest: &pkg.manifest,
            signer: &pkg.signer,
            previously_approved: None,
        };
        if !approve(&req) {
            return Err(Error::Unauthorized(format!(
                "the user did not approve the capabilities of `{}`",
                pkg.manifest.id
            )));
        }
        let record = self.commit(&pkg, pkg.manifest.capabilities.clone(), true)?;
        index.plugins.insert(pkg.manifest.id.clone(), record);
        self.write_index(&index)?;
        self.gc_locked(&index)?;
        self.installed(&pkg.manifest.id, &index)
    }

    /// Replace an installed plugin with a newer version.
    pub fn update(
        &self,
        package: &PluginPackage,
        trusted: &[VerifyingKey],
        approve: &mut dyn FnMut(&ApprovalRequest<'_>) -> bool,
    ) -> Result<InstalledPlugin> {
        let pkg = package.verify(trusted)?;
        let _g = self.guard();
        let mut index = self.read_index()?;
        let id = pkg.manifest.id.clone();
        let old = index
            .plugins
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("plugin `{id}` is not installed")))?;
        self.check_seal(&id, &old)?;
        check_update(&pkg, &old)?;
        let requested = &pkg.manifest.capabilities;
        let approved = if requested.is_subset_of(&old.approved) {
            requested.clone()
        } else {
            let req = ApprovalRequest {
                manifest: &pkg.manifest,
                signer: &pkg.signer,
                previously_approved: Some(&old.approved),
            };
            if !approve(&req) {
                return Err(Error::Unauthorized(format!(
                    "the user did not approve the new capabilities of `{id}`"
                )));
            }
            requested.clone()
        };
        let record = self.commit(&pkg, approved, old.enabled)?;
        index.plugins.insert(id.clone(), record);
        self.write_index(&index)?;
        self.gc_locked(&index)?;
        self.installed(&id, &index)
    }

    /// Verify `package` and report what [`install`](Self::install) (when the plugin is not
    /// installed) or [`update`](Self::update) (when it is) would do. Writes nothing. Fails as
    /// those would on a bad signature, an invalid manifest, a downgrade or a different signer.
    pub fn preview(
        &self,
        package: &PluginPackage,
        trusted: &[VerifyingKey],
    ) -> Result<InstallPreview> {
        let pkg = package.verify(trusted)?;
        let _g = self.guard();
        let index = self.read_index()?;
        let (installed_version, previously_approved, needs_approval) =
            match index.plugins.get(&pkg.manifest.id) {
                None => (None, None, true),
                Some(old) => {
                    self.check_seal(&pkg.manifest.id, old)?;
                    let old_version = check_update(&pkg, old)?;
                    let needs = !pkg.manifest.capabilities.is_subset_of(&old.approved);
                    (Some(old_version), Some(old.approved.clone()), needs)
                }
            };
        Ok(InstallPreview {
            manifest: pkg.manifest,
            signer: pkg.signer,
            installed_version,
            previously_approved,
            needs_approval,
        })
    }

    /// Turn an installed plugin on or off. Returns `false` when it is not installed.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        let _g = self.guard();
        let mut index = self.read_index()?;
        let Some(rec) = index.plugins.get_mut(id) else {
            return Ok(false);
        };
        if rec.enabled != enabled {
            rec.enabled = enabled;
            self.write_index(&index)?;
        }
        Ok(true)
    }

    /// Uninstall a plugin and delete its state. Returns whether it was installed.
    pub fn remove(&self, id: &str) -> Result<bool> {
        let _g = self.guard();
        let mut index = self.read_index()?;
        if index.plugins.remove(id).is_none() {
            return Ok(false);
        }
        self.write_index(&index)?;
        // `id` came from the index, so it is a validated plugin id (no path separators).
        let state = self.state_root().join(id);
        if state.exists() {
            std::fs::remove_dir_all(state)?;
        }
        self.gc_locked(&index)?;
        Ok(true)
    }

    pub fn list(&self) -> Result<Vec<InstalledPlugin>> {
        let _g = self.guard();
        let index = self.read_index()?;
        index
            .plugins
            .keys()
            .map(|id| self.installed(id, &index))
            .collect()
    }

    pub fn get(&self, id: &str) -> Result<Option<InstalledPlugin>> {
        let _g = self.guard();
        let index = self.read_index()?;
        if !index.plugins.contains_key(id) {
            return Ok(None);
        }
        self.installed(id, &index).map(Some)
    }

    /// Read an installed plugin for loading: check the approval seal, re-hash both blobs and
    /// re-check the signature against `trusted`. Refused when no trusted key signed the package
    /// (for example, its key was removed from the trusted list), when the recorded signer or
    /// approved capabilities were edited, or when the record predates approval seals.
    pub fn load_trusted(&self, id: &str, trusted: &[VerifyingKey]) -> Result<VerifiedPlugin> {
        self.load_inner(id, Some(trusted))
    }

    /// Like [`load_trusted`](Self::load_trusted), but checks the signature against the signer
    /// recorded at install instead of a trusted-key list. The approval seal still binds that
    /// signer and the approved capabilities to the package. Prefer `load_trusted`.
    pub fn load(&self, id: &str) -> Result<VerifiedPlugin> {
        self.load_inner(id, None)
    }

    fn load_inner(&self, id: &str, trusted: Option<&[VerifyingKey]>) -> Result<VerifiedPlugin> {
        let _g = self.guard();
        let index = self.read_index()?;
        let rec = index
            .plugins
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("plugin `{id}` is not installed")))?;
        self.check_seal(id, rec)?;
        let manifest = self.read_blob(&rec.manifest_sha256)?;
        let wasm = self.read_blob(&rec.wasm_sha256)?;
        let signature = decode_signature(rec.signature.as_bytes())?;
        let signer = parse_verifying_key(&rec.signer)?;
        if let Some(trusted) = trusted
            && !trusted.contains(&signer)
        {
            return Err(Error::Integrity(format!(
                "plugin `{id}` is signed by {}, which is not a trusted key",
                rec.signer
            )));
        }
        verify_signature(&manifest, &wasm, &signature, &signer)?;
        let installed = self.installed(id, &index)?;
        Ok(VerifiedPlugin { installed, wasm })
    }

    /// Check that `rec` carries a valid approval seal.
    fn check_seal(&self, id: &str, rec: &Record) -> Result<()> {
        let Some(seal) = &rec.approval_seal else {
            return Err(Error::Integrity(format!(
                "plugin `{id}` was installed before approvals were sealed; \
                 re-approve the plugin (remove it and install it again)"
            )));
        };
        let expected = approval_seal(
            &self.approval_key,
            id,
            &rec.manifest_sha256,
            &rec.wasm_sha256,
            &rec.signer,
            &rec.approved,
        )?;
        if !constant_time_eq(seal.as_bytes(), expected.as_bytes()) {
            return Err(Error::Integrity(format!(
                "the approval record of plugin `{id}` was modified; \
                 re-approve the plugin (remove it and install it again)"
            )));
        }
        Ok(())
    }

    fn commit(
        &self,
        pkg: &VerifiedPackage,
        approved: Capabilities,
        enabled: bool,
    ) -> Result<Record> {
        self.write_blob(&pkg.manifest_sha256, &pkg.manifest_bytes)?;
        self.write_blob(&pkg.wasm_sha256, &pkg.wasm)?;
        let signer = hex::encode(pkg.signer.as_bytes());
        let approval_seal = approval_seal(
            &self.approval_key,
            &pkg.manifest.id,
            &pkg.manifest_sha256,
            &pkg.wasm_sha256,
            &signer,
            &approved,
        )?;
        Ok(Record {
            version: pkg.manifest.version.to_string(),
            manifest_sha256: pkg.manifest_sha256.clone(),
            wasm_sha256: pkg.wasm_sha256.clone(),
            signature: hex::encode(pkg.signature.to_bytes()),
            signer,
            approved,
            approval_seal: Some(approval_seal),
            installed_at: OffsetDateTime::now_utc(),
            enabled,
        })
    }

    fn installed(&self, id: &str, index: &Index) -> Result<InstalledPlugin> {
        let rec = index
            .plugins
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("plugin `{id}` is not installed")))?;
        let manifest = Manifest::parse(&self.read_blob(&rec.manifest_sha256)?)?;
        if manifest.id != id {
            return Err(Error::Integrity(format!(
                "index entry `{id}` points at the manifest of `{}`",
                manifest.id
            )));
        }
        Ok(InstalledPlugin {
            manifest,
            approved: rec.approved.clone(),
            wasm_sha256: rec.wasm_sha256.clone(),
            manifest_sha256: rec.manifest_sha256.clone(),
            signer: rec.signer.clone(),
            installed_at: rec.installed_at,
            enabled: rec.enabled,
        })
    }

    fn blob_path(&self, sha: &str) -> Result<PathBuf> {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Storage(format!("bad blob hash `{sha}`")));
        }
        Ok(self.root.join("blobs").join("sha256").join(sha))
    }

    fn read_blob(&self, sha: &str) -> Result<Vec<u8>> {
        let bytes = std::fs::read(self.blob_path(sha)?)?;
        if sha256_hex(&bytes) != sha {
            return Err(Error::Integrity(format!(
                "plugin blob {sha} was modified on disk"
            )));
        }
        Ok(bytes)
    }

    fn write_blob(&self, sha: &str, bytes: &[u8]) -> Result<()> {
        let path = self.blob_path(sha)?;
        if path.exists() && self.read_blob(sha).is_ok() {
            return Ok(());
        }
        write_atomic(&path, bytes)
    }

    /// Delete blobs no index entry references.
    fn gc_locked(&self, index: &Index) -> Result<()> {
        let live: std::collections::BTreeSet<&str> = index
            .plugins
            .values()
            .flat_map(|r| [r.manifest_sha256.as_str(), r.wasm_sha256.as_str()])
            .collect();
        for entry in std::fs::read_dir(self.root.join("blobs").join("sha256"))? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !live.contains(name.as_ref()) {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    fn read_index(&self) -> Result<Index> {
        match std::fs::read(self.index_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::Storage(format!("plugin index is corrupt: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Index::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn write_index(&self, index: &Index) -> Result<()> {
        write_atomic(&self.index_path(), &serde_json::to_vec_pretty(index)?)
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The rules an update must follow: a newer version, signed by the installed version's key.
/// Returns the installed version.
fn check_update(pkg: &VerifiedPackage, old: &Record) -> Result<semver::Version> {
    let id = &pkg.manifest.id;
    let old_version = semver::Version::parse(&old.version)
        .map_err(|e| Error::Storage(format!("index: bad version: {e}")))?;
    if pkg.manifest.version <= old_version {
        return Err(Error::Invalid(format!(
            "update of `{id}` must raise the version ({} is not newer than {old_version})",
            pkg.manifest.version
        )));
    }
    if hex::encode(pkg.signer.as_bytes()) != old.signer {
        return Err(Error::Integrity(format!(
            "update of `{id}` is signed by a different key than the installed version"
        )));
    }
    Ok(old_version)
}

/// HMAC-SHA256 (RFC 2104) of the approval of `id` under `key`, as lowercase hex: binds the
/// approved capabilities to the exact package (both blob hashes) and its signer.
fn approval_seal(
    key: &[u8; 32],
    id: &str,
    manifest_sha256: &str,
    wasm_sha256: &str,
    signer: &str,
    approved: &Capabilities,
) -> Result<String> {
    // `Capabilities` serializes with a fixed field order, so this JSON is canonical.
    let caps = serde_json::to_string(approved)?;
    let message = format!(
        "nugget-plugin-approval/v1\nid:{id}\nmanifest-sha256:{manifest_sha256}\n\
         wasm-sha256:{wasm_sha256}\nsigner:{signer}\ncapabilities:{caps}\n"
    );
    Ok(hex::encode(hmac_sha256(key, message.as_bytes())))
}

fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    // The key is shorter than the 64-byte block, so it is zero-padded as is.
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for (i, b) in key.iter().enumerate() {
        ipad[i] ^= b;
        opad[i] ^= b;
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize()
        .into()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Read the approval key, or create it with fresh random bytes (0600 on unix).
fn load_or_create_key(path: &Path) -> Result<[u8; 32]> {
    let read = |path: &Path| -> Result<[u8; 32]> {
        std::fs::read(path)?
            .try_into()
            .map_err(|_| Error::Integrity(format!("{} is not a 32-byte key", path.display())))
    };
    if path.exists() {
        return read(path);
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key)
        .map_err(|e| Error::Storage(format!("cannot generate the approval key: {e}")))?;
    // Write a private temporary file, then link it into place so a concurrent opener never
    // sees a partial key; whoever links first wins and the other reads that key.
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&key)?;
        f.sync_all()?;
    }
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(key),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read(path),
        Err(e) => Err(e.into()),
    }
}

/// Write to a temporary sibling, then rename over `path`.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::Storage("path has no parent".into()))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}
