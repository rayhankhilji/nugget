//! Signed model manifest.
//!
//! The manifest is a JSON document signed with a **detached ed25519 signature over its exact bytes**.
//! Nothing in it may be used before [`verify_manifest`] has checked the signature against a trusted
//! key and validated every entry (pinned revision, well-formed hash, safe URL and file name).

use std::collections::HashSet;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use nugget_core::capability::Tier;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use url::Url;

/// Hex-encoded ed25519 public key of the **development** manifest signing key.
///
/// The matching private key is never committed. Release builds must replace or extend this with a
/// release key held offline (see the crate docs).
pub const DEV_PUBLIC_KEY_HEX: &str =
    "a56054ce95a98642b081f19398960b1e933a4e9f3f22079dc272630bf681d9b3";

/// The manifest shipped with this build (`manifests/default.json`) and its detached signature.
pub const DEFAULT_MANIFEST_JSON: &[u8] = include_bytes!("../../../manifests/default.json");
/// Hex-encoded detached signature over [`DEFAULT_MANIFEST_JSON`].
pub const DEFAULT_MANIFEST_SIG: &[u8] = include_bytes!("../../../manifests/default.json.sig");

/// A verified list of downloadable models. Obtain one only through [`verify_manifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Monotonically increasing manifest serial. Callers should refuse a manifest whose version is
    /// lower than the last one they accepted (freeze/rollback protection).
    pub version: u64,
    pub models: Vec<ModelEntry>,
    /// Pinned inference-runtime builds (`llama-server`), one per OS/arch/accelerator. Optional so
    /// manifests without runtimes stay valid; omitted from the JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<crate::runtime::RuntimeEntry>,
}

/// One downloadable model file, pinned to an exact upstream revision and content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    /// Stable id used for slots and config, e.g. `qwen3-4b-q4km`. `[a-z0-9._-]`, 1–64 chars.
    pub id: String,
    /// Model family, e.g. `qwen3`; informational and used for runtime selection.
    pub family: String,
    /// Upstream file name, e.g. `Qwen3-4B-Q4_K_M.gguf`. Never contains a path separator.
    pub file_name: String,
    /// Primary download URL. Must reference [`ModelEntry::revision`] (never a moving ref).
    pub url: String,
    /// Alternative URLs serving the identical bytes, tried in order if `url` is unusable.
    #[serde(default)]
    pub mirrors: Vec<String>,
    /// Exact upstream commit (40 or 64 hex chars). Branch names such as `main` are rejected.
    pub revision: String,
    /// Exact size in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
    /// Minimum total system RAM (GiB) for this model to be recommended.
    pub min_ram_gb: u32,
    /// Workload tiers this model can serve.
    pub tiers: Vec<Tier>,
    /// SPDX license id of the model weights, e.g. `Apache-2.0`.
    pub license: String,
    /// Minimum Nugget version (semver) able to run this model.
    pub min_app_version: String,
}

impl Manifest {
    /// Look up an entry by id.
    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }
}

impl ModelEntry {
    /// All download sources in preference order: `url`, then `mirrors`.
    pub fn sources(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.url.as_str()).chain(self.mirrors.iter().map(String::as_str))
    }

    /// Whether this entry can run on an app of version `app_version`.
    pub fn compatible_with(&self, app_version: &semver::Version) -> bool {
        semver::Version::parse(&self.min_app_version).is_ok_and(|min| *app_version >= min)
    }

    /// Validate an entry in isolation. [`verify_manifest`] calls this for every entry; the store
    /// calls it again before installing so a hand-built entry cannot bypass the rules.
    pub fn validate(&self) -> Result<()> {
        let id = &self.id;
        let bad = |what: String| Err(Error::Invalid(format!("model `{id}`: {what}")));

        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
            || id.starts_with('.')
        {
            return bad("id must be 1-64 chars of [a-z0-9._-] and not start with '.'".into());
        }
        if self.family.trim().is_empty() {
            return bad("family is empty".into());
        }
        if !is_safe_file_name(&self.file_name) {
            return bad(format!("unsafe file_name `{}`", self.file_name));
        }
        if !is_commit_hash(&self.revision) {
            return bad(format!(
                "revision `{}` is not an exact commit hash (moving refs like `main` are not allowed)",
                self.revision
            ));
        }
        if self.size == 0 {
            return bad("size is zero".into());
        }
        if !is_sha256_hex(&self.sha256) {
            return bad("sha256 must be 64 lowercase hex chars".into());
        }
        if self.tiers.is_empty() {
            return bad("tiers is empty".into());
        }
        if self.license.trim().is_empty() {
            return bad("license is empty".into());
        }
        if semver::Version::parse(&self.min_app_version).is_err() {
            return bad(format!(
                "min_app_version `{}` is not semver",
                self.min_app_version
            ));
        }
        for src in self.sources() {
            validate_url(src, &self.revision).or_else(bad)?;
        }
        Ok(())
    }
}

/// Verify a detached signature over `bytes` with any of `keys`, then parse and validate the
/// manifest. This is the only supported way to obtain a [`Manifest`] from untrusted bytes.
///
/// `sig` may be the raw 64-byte signature or its hex encoding (surrounding whitespace ignored).
pub fn verify_manifest(bytes: &[u8], sig: &[u8], keys: &[VerifyingKey]) -> Result<Manifest> {
    if keys.is_empty() {
        return Err(Error::Integrity(
            "no trusted manifest keys configured".into(),
        ));
    }
    let sig = decode_signature(sig)?;
    if !keys.iter().any(|k| k.verify_strict(bytes, &sig).is_ok()) {
        return Err(Error::Integrity(
            "manifest signature does not match any trusted key".into(),
        ));
    }
    let manifest: Manifest = serde_json::from_slice(bytes)
        .map_err(|e| Error::Invalid(format!("manifest is not valid JSON: {e}")))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// The manifest bundled with this build, verified against [`trusted_keys`].
pub fn default_manifest() -> Result<Manifest> {
    verify_manifest(
        DEFAULT_MANIFEST_JSON,
        DEFAULT_MANIFEST_SIG,
        &trusted_keys()?,
    )
}

/// Keys trusted to sign manifests in this build.
pub fn trusted_keys() -> Result<Vec<VerifyingKey>> {
    Ok(vec![parse_verifying_key(DEV_PUBLIC_KEY_HEX)?])
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

/// Sign `bytes` and return the detached signature as lowercase hex (the `.sig` file format).
pub fn sign_manifest(bytes: &[u8], key: &SigningKey) -> String {
    hex::encode(key.sign(bytes).to_bytes())
}

/// Decode a detached signature given as 64 raw bytes or as hex text.
pub fn decode_signature(sig: &[u8]) -> Result<Signature> {
    if sig.len() == 64 {
        let arr: [u8; 64] = sig
            .try_into()
            .map_err(|_| Error::Integrity("bad sig".into()))?;
        return Ok(Signature::from_bytes(&arr));
    }
    let text = std::str::from_utf8(sig)
        .map_err(|_| Error::Integrity("signature is neither 64 raw bytes nor hex text".into()))?;
    let arr: [u8; 64] =
        decode_hex_array(text.trim(), "signature").map_err(|e| Error::Integrity(e.to_string()))?;
    Ok(Signature::from_bytes(&arr))
}

fn validate_manifest(m: &Manifest) -> Result<()> {
    let mut ids = HashSet::new();
    for entry in &m.models {
        entry.validate()?;
        if !ids.insert(entry.id.as_str()) {
            return Err(Error::Invalid(format!("duplicate model id `{}`", entry.id)));
        }
    }
    crate::runtime::validate_runtimes(m)
}

fn decode_hex_array<const N: usize>(s: &str, what: &str) -> Result<[u8; N]> {
    let v = hex::decode(s).map_err(|e| Error::Invalid(format!("{what}: invalid hex: {e}")))?;
    v.try_into().map_err(|v: Vec<u8>| {
        Error::Invalid(format!("{what}: expected {N} bytes, got {}", v.len()))
    })
}

fn is_hex_lower(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && is_hex_lower(s)
}

fn is_commit_hash(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && is_hex_lower(s)
}

fn is_safe_file_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s != "."
        && s != ".."
        && !s.contains(['/', '\\', ':', '\0'])
        && !s.chars().any(char::is_control)
}

/// URL rules: `https` everywhere; plain `http` only to a loopback host (dev/tests). The URL must
/// reference the pinned revision so it can never resolve to a moving ref.
fn validate_url(raw: &str, revision: &str) -> std::result::Result<(), String> {
    let url = Url::parse(raw).map_err(|e| format!("invalid url `{raw}`: {e}"))?;
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => return Err(format!("url `{raw}` has no host")),
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        other => {
            return Err(format!(
                "url `{raw}`: scheme `{other}` not allowed (https only)"
            ));
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("url `{raw}` must not embed credentials"));
    }
    if !url.path().contains(revision) {
        return Err(format!(
            "url `{raw}` does not reference pinned revision `{revision}`"
        ));
    }
    Ok(())
}
