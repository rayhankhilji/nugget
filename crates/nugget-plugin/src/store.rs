//! Installed plugins: a content-addressed directory under the data dir.
//!
//! ```text
//! <data>/plugins/
//!   blobs/sha256/<hex>        manifests and components, by content hash
//!   index.json                id -> installed version, hashes, signature, approved capabilities
//!   state/<id>/               the plugin's key-value store (see `kv`)
//! ```
//!
//! Install and update verify the package signature against trusted keys, then ask the user to
//! approve the declared capabilities. An update must raise the version (no downgrades), must be
//! signed by the same key as the installed version, and asks again only when it requests more
//! than was approved before. Loading re-hashes the blobs and re-checks the signature, so a
//! component altered on disk is refused.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    plugins: BTreeMap<String, Record>,
}

/// The installed-plugin directory.
#[derive(Debug)]
pub struct PluginStore {
    root: PathBuf,
    lock: Mutex<()>,
}

impl PluginStore {
    /// The store at `<data>/plugins`.
    pub fn open(paths: &Paths) -> Result<Self> {
        Self::open_at(paths.data.join("plugins"))
    }

    pub fn open_at(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(root.join("blobs").join("sha256"))?;
        std::fs::create_dir_all(root.join("state"))?;
        Ok(Self {
            root,
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
        let record = self.commit(&pkg, pkg.manifest.capabilities.clone())?;
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
        let record = self.commit(&pkg, approved)?;
        index.plugins.insert(id.clone(), record);
        self.write_index(&index)?;
        self.gc_locked(&index)?;
        self.installed(&id, &index)
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

    /// Read an installed plugin for loading: re-hash both blobs and re-check the signature.
    pub fn load(&self, id: &str) -> Result<VerifiedPlugin> {
        let _g = self.guard();
        let index = self.read_index()?;
        let rec = index
            .plugins
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("plugin `{id}` is not installed")))?;
        let manifest = self.read_blob(&rec.manifest_sha256)?;
        let wasm = self.read_blob(&rec.wasm_sha256)?;
        let signature = decode_signature(rec.signature.as_bytes())?;
        let signer = parse_verifying_key(&rec.signer)?;
        verify_signature(&manifest, &wasm, &signature, &signer)?;
        let installed = self.installed(id, &index)?;
        Ok(VerifiedPlugin { installed, wasm })
    }

    fn commit(&self, pkg: &VerifiedPackage, approved: Capabilities) -> Result<Record> {
        self.write_blob(&pkg.manifest_sha256, &pkg.manifest_bytes)?;
        self.write_blob(&pkg.wasm_sha256, &pkg.wasm)?;
        Ok(Record {
            version: pkg.manifest.version.to_string(),
            manifest_sha256: pkg.manifest_sha256.clone(),
            wasm_sha256: pkg.wasm_sha256.clone(),
            signature: hex::encode(pkg.signature.to_bytes()),
            signer: hex::encode(pkg.signer.as_bytes()),
            approved,
            installed_at: OffsetDateTime::now_utc(),
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
