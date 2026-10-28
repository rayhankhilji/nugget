//! Verified local model distribution and hardware-aware model choice.
//!
//! Trust chain, in order:
//! 1. **Signed manifest.** [`verify_manifest`] checks a detached ed25519 signature over the exact
//!    manifest bytes against trusted keys, then validates every entry: an exact upstream commit
//!    (never a moving ref such as `main`), exact size, SHA-256, a URL that references that
//!    commit, and a safe file name.
//! 2. **Fail-closed preflight.** [`ModelStore::install`] refuses to start unless free space is at
//!    least the remaining bytes + 10% of the size + 1 GiB, and treats an unreadable free-space value
//!    as an error.
//! 3. **Resumable parallel download** with per-part retry and backoff, into a staging directory.
//! 4. **Streaming SHA-256** of the staged file; a mismatch wipes the staging directory.
//! 5. **Atomic install** into content-addressed storage (`blobs/sha256/<hex>`), then a slot update
//!    that keeps the previous version for [`ModelStore::rollback`]. Egress is recorded.
//!
//! Installed models are re-checkable at any time ([`ModelStore::verify`]), and
//! [`ModelStore::needs_update`] tells whether a newer manifest pins different bytes.
//!
//! ## Signing keys
//! This build trusts only the **development** key [`DEV_PUBLIC_KEY_HEX`]. Sign manifests with
//! `cargo run -p nugget-models --example sign-manifest`; the private key is read from
//! `NUGGET_MANIFEST_SIGNING_KEY` (hex) or a key file and is never committed.

mod download;
mod hardware;
mod manifest;
mod preflight;
mod store;

pub use download::{DownloadOptions, EGRESS_FEATURE, Phase, Progress};
pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
pub use hardware::{GpuKind, Hardware, detect_hardware, recommend};
pub use manifest::{
    DEFAULT_MANIFEST_JSON, DEFAULT_MANIFEST_SIG, DEV_PUBLIC_KEY_HEX, Manifest, ModelEntry,
    decode_signature, default_manifest, parse_signing_key, parse_verifying_key, sign_manifest,
    trusted_keys, verify_manifest,
};
pub use preflight::{DiskSpace, GIB, SystemDiskSpace, check_disk_space, required_bytes};
pub use semver;
pub use store::{GcReport, InstalledModel, ModelStore, Slot, default_client};

/// Version of this build, compared against [`ModelEntry::min_app_version`].
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
