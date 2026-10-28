//! Signed plugin packages.
//!
//! A package is a directory holding [`MANIFEST_FILE`], [`WASM_FILE`] (a WebAssembly component)
//! and [`SIGNATURE_FILE`], a detached **ed25519** signature in hex. As with the model manifest in
//! `nugget-models`, the signature is checked before anything in the package is parsed or used.
//! It covers both files through [`signing_payload`]: a domain-separation tag plus the SHA-256 of
//! the exact manifest bytes and of the exact component bytes, so neither can be swapped.

use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use nugget_core::{Error, Result};
use sha2::{Digest, Sha256};

use crate::manifest::{MANIFEST_FILE, Manifest};

/// Component file name inside a package directory.
pub const WASM_FILE: &str = "plugin.wasm";
/// Detached signature file name inside a package directory.
pub const SIGNATURE_FILE: &str = "plugin.sig";
/// Largest accepted component.
pub const MAX_WASM_BYTES: usize = 32 * 1024 * 1024;
const PAYLOAD_TAG: &str = "nugget-plugin-package/v1";

/// The raw files of a package. Untrusted until [`verify`](Self::verify) succeeds.
#[derive(Debug, Clone)]
pub struct PluginPackage {
    pub manifest: Vec<u8>,
    pub wasm: Vec<u8>,
    pub signature: Vec<u8>,
}

/// A package whose signature matched a trusted key and whose manifest is valid.
#[derive(Debug, Clone)]
pub struct VerifiedPackage {
    pub manifest: Manifest,
    pub manifest_bytes: Vec<u8>,
    pub wasm: Vec<u8>,
    pub signature: Signature,
    pub signer: VerifyingKey,
    /// Lowercase hex SHA-256 of the component.
    pub wasm_sha256: String,
    /// Lowercase hex SHA-256 of the manifest.
    pub manifest_sha256: String,
}

impl PluginPackage {
    /// Read a package directory.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let read = |name: &str, max: usize| -> Result<Vec<u8>> {
            let path = dir.join(name);
            let len = std::fs::metadata(&path)?.len();
            if len > max as u64 {
                return Err(Error::Invalid(format!("{name} is larger than {max} bytes")));
            }
            Ok(std::fs::read(path)?)
        };
        Ok(Self {
            manifest: read(MANIFEST_FILE, crate::manifest::MAX_MANIFEST_BYTES)?,
            wasm: read(WASM_FILE, MAX_WASM_BYTES)?,
            signature: read(SIGNATURE_FILE, 1024)?,
        })
    }

    /// Write the package into `dir` (created if missing).
    pub fn write_dir(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(MANIFEST_FILE), &self.manifest)?;
        std::fs::write(dir.join(WASM_FILE), &self.wasm)?;
        std::fs::write(dir.join(SIGNATURE_FILE), &self.signature)?;
        Ok(())
    }

    /// Sign `manifest` and `wasm` with `key` and bundle them.
    pub fn sign(manifest: Vec<u8>, wasm: Vec<u8>, key: &SigningKey) -> Self {
        let signature = sign_package(&manifest, &wasm, key).into_bytes();
        Self {
            manifest,
            wasm,
            signature,
        }
    }

    /// Check the signature against `trusted` keys, then parse and validate the manifest and
    /// check the component header. Nothing is parsed before the signature matches.
    pub fn verify(&self, trusted: &[VerifyingKey]) -> Result<VerifiedPackage> {
        if trusted.is_empty() {
            return Err(Error::Integrity("no trusted plugin signing keys".into()));
        }
        if self.wasm.len() > MAX_WASM_BYTES {
            return Err(Error::Invalid(format!(
                "component is larger than {MAX_WASM_BYTES} bytes"
            )));
        }
        let signature = decode_signature(&self.signature)?;
        let payload = signing_payload(&self.manifest, &self.wasm);
        let signer = trusted
            .iter()
            .find(|k| k.verify_strict(&payload, &signature).is_ok())
            .copied()
            .ok_or_else(|| {
                Error::Integrity("plugin signature does not match any trusted key".into())
            })?;
        let manifest = Manifest::parse(&self.manifest)?;
        check_component_header(&self.wasm)?;
        Ok(VerifiedPackage {
            manifest,
            manifest_bytes: self.manifest.clone(),
            wasm: self.wasm.clone(),
            signature,
            signer,
            wasm_sha256: sha256_hex(&self.wasm),
            manifest_sha256: sha256_hex(&self.manifest),
        })
    }
}

/// The bytes a package signature covers.
pub fn signing_payload(manifest: &[u8], wasm: &[u8]) -> Vec<u8> {
    format!(
        "{PAYLOAD_TAG}\nmanifest-sha256:{}\nwasm-sha256:{}\n",
        sha256_hex(manifest),
        sha256_hex(wasm)
    )
    .into_bytes()
}

/// Sign a package and return the detached signature as lowercase hex (the `plugin.sig` format).
pub fn sign_package(manifest: &[u8], wasm: &[u8], key: &SigningKey) -> String {
    hex::encode(key.sign(&signing_payload(manifest, wasm)).to_bytes())
}

/// Check a stored signature again (for a package already in the store).
pub(crate) fn verify_signature(
    manifest: &[u8],
    wasm: &[u8],
    signature: &Signature,
    signer: &VerifyingKey,
) -> Result<()> {
    signer
        .verify_strict(&signing_payload(manifest, wasm), signature)
        .map_err(|_| Error::Integrity("stored plugin no longer matches its signature".into()))
}

/// Parse a hex-encoded 32-byte ed25519 public key.
pub fn parse_verifying_key(hex_key: &str) -> Result<VerifyingKey> {
    let bytes: [u8; 32] = decode_hex_array(hex_key.trim(), "public key")?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|e| Error::Invalid(format!("invalid ed25519 public key: {e}")))
}

/// Parse a hex-encoded 32-byte ed25519 secret seed (signing tooling and tests only).
pub fn parse_signing_key(hex_seed: &str) -> Result<SigningKey> {
    let mut bytes: [u8; 32] = decode_hex_array(hex_seed.trim(), "signing key")?;
    let key = SigningKey::from_bytes(&bytes);
    bytes.fill(0);
    Ok(key)
}

/// Decode a detached signature given as 64 raw bytes or as hex text.
pub fn decode_signature(sig: &[u8]) -> Result<Signature> {
    if let Ok(arr) = <[u8; 64]>::try_from(sig) {
        return Ok(Signature::from_bytes(&arr));
    }
    let text = std::str::from_utf8(sig)
        .map_err(|_| Error::Integrity("signature is neither 64 raw bytes nor hex text".into()))?;
    let arr: [u8; 64] =
        decode_hex_array(text.trim(), "signature").map_err(|e| Error::Integrity(e.to_string()))?;
    Ok(Signature::from_bytes(&arr))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn decode_hex_array<const N: usize>(text: &str, what: &str) -> Result<[u8; N]> {
    let bytes = hex::decode(text).map_err(|e| Error::Invalid(format!("{what}: bad hex: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| Error::Invalid(format!("{what}: expected {N} bytes")))
}

/// A WebAssembly *component* (not a core module): `\0asm`, then layer 1.
fn check_component_header(wasm: &[u8]) -> Result<()> {
    if wasm.len() < 8 || &wasm[..4] != b"\0asm" || wasm[6..8] != [1, 0] {
        return Err(Error::Invalid(
            "plugin.wasm is not a WebAssembly component".into(),
        ));
    }
    Ok(())
}
