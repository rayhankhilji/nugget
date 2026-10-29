//! Manifest rollback (freeze) protection.
//!
//! [`Manifest::version`] is a monotonic serial. [`ManifestTrust`] persists, **per trusted signing
//! key**, the highest version this installation has accepted, in a small JSON file in the model
//! store root (`<models_dir>/manifest-trust.json`). A manifest signed by a key whose recorded
//! version is higher is refused, even though its signature is valid: that is what stops an
//! attacker (or a stale mirror) from replaying an old, correctly signed manifest that pins
//! vulnerable or withdrawn files.
//!
//! - **Fresh install** (no state file, or no entry for the key): accepted and recorded.
//! - **Equal version**: accepted (the same manifest is loaded on every start).
//! - **Higher version**: accepted, and the new version is recorded.
//! - **Lower version**: refused with [`Error::Integrity`].
//! - **Corrupt state file**: fails closed with [`Error::Integrity`]; nothing is accepted until the
//!   file is fixed or removed (removing it resets rollback protection to a fresh install).
//!
//! Versions are tracked per key because a release key and the development key sign independent
//! manifest series; one key's serial says nothing about another's.
//!
//! The file is written atomically (temporary file in the same directory, `fsync`, rename).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::VerifyingKey;
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::manifest::{
    DEFAULT_MANIFEST_JSON, DEFAULT_MANIFEST_SIG, Manifest, trusted_keys, verify_manifest_with_key,
};

/// File name of the rollback-protection state inside the model store root.
pub const MANIFEST_TRUST_FILE: &str = "manifest-trust.json";

const FORMAT: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustState {
    format: u32,
    /// Lowercase hex public key -> highest accepted manifest version.
    keys: BTreeMap<String, u64>,
}

/// Persisted highest-accepted manifest version per trusted key. See the module docs.
#[derive(Debug)]
pub struct ManifestTrust {
    path: PathBuf,
    /// Serialises read-check-write within this process.
    lock: Mutex<()>,
}

impl ManifestTrust {
    /// Open the state kept in `root` (the model store root). The file need not exist yet; an
    /// existing but unreadable or corrupt file is an error (fail closed).
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let trust = Self {
            path: root.as_ref().join(MANIFEST_TRUST_FILE),
            lock: Mutex::new(()),
        };
        trust.load()?;
        Ok(trust)
    }

    /// Open the state in this installation's model store root ([`Paths::models_dir`]).
    pub fn for_paths(paths: &Paths) -> Result<Self> {
        Self::open(paths.models_dir())
    }

    /// Path of the state file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Highest version accepted so far from `key`, if any.
    pub fn accepted_version(&self, key: &VerifyingKey) -> Result<Option<u64>> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.load()?.keys.get(&key_hex(key)).copied())
    }

    /// Refuse `version` from `key` if it is older than the version already accepted from that key;
    /// otherwise accept it and record it if it is newer (or the first one seen).
    pub fn check_and_record(&self, key: &VerifyingKey, version: u64) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.load()?;
        let hex = key_hex(key);
        match state.keys.get(&hex) {
            Some(&seen) if version < seen => Err(Error::Integrity(format!(
                "manifest version {version} is older than version {seen} already accepted from \
                 key {}…; refusing a rollback",
                &hex[..16]
            ))),
            Some(&seen) if version == seen => Ok(()),
            prev => {
                if let Some(seen) = prev {
                    tracing::info!(key = %&hex[..16], from = seen, to = version, "manifest version advanced");
                }
                state.keys.insert(hex, version);
                state.format = FORMAT;
                self.store(&state)
            }
        }
    }

    fn load(&self) -> Result<TrustState> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TrustState {
                    format: FORMAT,
                    keys: BTreeMap::new(),
                });
            }
            Err(e) => {
                return Err(Error::Integrity(format!(
                    "cannot read manifest trust state {}: {e}",
                    self.path.display()
                )));
            }
        };
        let corrupt = |why: String| {
            Error::Integrity(format!(
                "manifest trust state {} is corrupt ({why}); refusing to accept any manifest. \
                 Removing the file resets rollback protection",
                self.path.display()
            ))
        };
        let state: TrustState =
            serde_json::from_slice(&bytes).map_err(|e| corrupt(e.to_string()))?;
        if state.format != FORMAT {
            return Err(corrupt(format!("unsupported format {}", state.format)));
        }
        if let Some(k) = state.keys.keys().find(|k| !is_key_hex(k)) {
            return Err(corrupt(format!("`{k}` is not a hex public key")));
        }
        Ok(state)
    }

    fn store(&self, state: &TrustState) -> Result<()> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = self
            .path
            .parent()
            .ok_or_else(|| Error::Invalid("manifest trust path has no parent".into()))?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            ".{MANIFEST_TRUST_FILE}.{}.{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(state)?)?;
            f.write_all(b"\n")?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, &self.path)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            Error::Storage(format!(
                "cannot write manifest trust state {}: {e}",
                self.path.display()
            ))
        })
    }
}

/// Verify a manifest ([`verify_manifest_with_key`]) and then apply rollback protection with
/// `trust` for the key that verified it. Use this for every manifest, embedded or downloaded.
pub fn verify_manifest_checked(
    bytes: &[u8],
    sig: &[u8],
    keys: &[VerifyingKey],
    trust: &ManifestTrust,
) -> Result<Manifest> {
    let (manifest, key) = verify_manifest_with_key(bytes, sig, keys)?;
    trust.check_and_record(&key, manifest.version)?;
    Ok(manifest)
}

/// The manifest bundled with this build, verified against [`trusted_keys`] and checked against
/// the rollback-protection state in this installation's model store.
pub fn default_manifest_checked(paths: &Paths) -> Result<Manifest> {
    let trust = ManifestTrust::for_paths(paths)?;
    verify_manifest_checked(
        DEFAULT_MANIFEST_JSON,
        DEFAULT_MANIFEST_SIG,
        &trusted_keys()?,
        &trust,
    )
}

fn key_hex(key: &VerifyingKey) -> String {
    hex::encode(key.to_bytes())
}

fn is_key_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
