//! `.nuggetplugin` packages: the single-file container, signature decoding and verification, and
//! the store's install/load round trip.
//!
//! Each input is treated as a whole package file:
//!
//! 1. `PluginPackage::from_bytes` on the raw bytes. An accepted file re-encodes to exactly the
//!    same bytes, and verification against the fixed trusted key is deterministic. Anything that
//!    verifies has a signer equal to the trusted key, hashes equal to the SHA-256 of its parts, and
//!    a manifest that re-parses to the same value. (The seeds are signed with that key, so libFuzzer
//!    starts from packages that verify.)
//! 2. The decoded manifest and component re-signed with the trusted key: such a package verifies
//!    exactly when the manifest parses and the component header is valid; signed by any other key,
//!    or with one bit of either part changed after signing, it never verifies. A package that
//!    verifies installs into a store, loads back with the same manifest and component, and is
//!    removed again.
//! 3. The detached-signature decoder (64 raw bytes decode to themselves) and the hex key parser on
//!    the raw bytes.
//!
//! The store lives in a per-process scratch directory under the system temp dir.
#![no_main]

use std::path::PathBuf;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use nugget_plugin::{
    Manifest, PluginPackage, PluginStore, SigningKey, VerifyingKey, decode_signature,
    parse_verifying_key,
};

/// The throwaway key the seed corpus is signed with (seed bytes 0x07 * 32). Public key:
/// ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c.
const TRUSTED_SEED: [u8; 32] = [7; 32];
const OTHER_SEED: [u8; 32] = [9; 32];

struct Fixture {
    trusted: SigningKey,
    other: SigningKey,
    store: PluginStore,
}

fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "nugget-fuzz-plugin-package-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let store =
            PluginStore::open_with_key(root, [3; 32]).expect("open a scratch plugin store");
        Fixture {
            trusted: SigningKey::from_bytes(&TRUSTED_SEED),
            other: SigningKey::from_bytes(&OTHER_SEED),
            store,
        }
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// What `verify` checks after the signature, restated independently.
fn component_header_ok(wasm: &[u8]) -> bool {
    wasm.len() >= 8 && wasm[..4] == *b"\0asm" && wasm[6..8] == [1, 0]
}

fn check_verified(pkg: &PluginPackage, key: &VerifyingKey) -> Option<nugget_plugin::VerifiedPackage> {
    let trusted = [*key];
    let first = pkg.verify(&trusted);
    let second = pkg.verify(&trusted);
    assert_eq!(first.is_ok(), second.is_ok(), "verification is deterministic");
    let (Ok(v), Ok(again)) = (first, second) else {
        return None;
    };
    assert_eq!(v.signer, *key);
    assert_eq!(v.signature, again.signature);
    assert_eq!(v.manifest, again.manifest);
    assert_eq!(v.manifest_bytes, pkg.manifest);
    assert_eq!(v.wasm, pkg.wasm);
    assert_eq!(v.wasm_sha256, sha256_hex(&pkg.wasm));
    assert_eq!(v.manifest_sha256, sha256_hex(&pkg.manifest));
    let reparsed = Manifest::parse(&v.manifest_bytes).expect("a verified manifest re-parses");
    assert_eq!(reparsed, v.manifest);
    assert!(component_header_ok(&v.wasm), "verified component has a valid header");
    // The signature is bound to the key: the same package is refused under any other key.
    let other = fixture().other.verifying_key();
    if other != *key {
        assert!(pkg.verify(&[other]).is_err(), "verified under an untrusted key");
    }
    assert!(pkg.verify(&[]).is_err(), "verified with no trusted keys");
    Some(v)
}

fuzz_target!(|data: &[u8]| {
    let fx = fixture();
    let trusted = fx.trusted.verifying_key();

    // 3. Signature and key decoding never panic; a decoded raw signature is the input.
    if let Ok(sig) = decode_signature(data)
        && data.len() == 64
    {
        assert_eq!(sig.to_bytes().as_slice(), data);
    }
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = parse_verifying_key(text);
    }

    // 1. The raw container.
    let Ok(pkg) = PluginPackage::from_bytes(data) else {
        return;
    };
    assert_eq!(pkg.to_bytes(), data, "an accepted package re-encodes to the same bytes");
    let decoded = PluginPackage::from_bytes(&pkg.to_bytes()).expect("re-encoded package decodes");
    assert_eq!(decoded.manifest, pkg.manifest);
    assert_eq!(decoded.wasm, pkg.wasm);
    assert_eq!(decoded.signature, pkg.signature);
    let _ = check_verified(&pkg, &trusted);

    // 2. Re-signed with the trusted key.
    let signed = PluginPackage::sign(pkg.manifest.clone(), pkg.wasm.clone(), &fx.trusted);
    let should_verify = Manifest::parse(&signed.manifest).is_ok() && component_header_ok(&signed.wasm);
    let verified = check_verified(&signed, &trusted);
    assert_eq!(
        verified.is_some(),
        should_verify,
        "a correctly signed package verifies exactly when its manifest and component are valid"
    );
    let by_other = PluginPackage::sign(pkg.manifest.clone(), pkg.wasm.clone(), &fx.other);
    assert!(by_other.verify(&[trusted]).is_err(), "verified a package signed by another key");

    // Any single bit flip in the manifest or the component breaks the signature.
    let pick = data.iter().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(*b as usize));
    if !signed.manifest.is_empty() {
        let mut m = signed.clone();
        let i = pick % m.manifest.len();
        m.manifest[i] ^= 1 << (pick % 8);
        assert!(m.verify(&[trusted]).is_err(), "manifest bit flip still verifies");
    }
    if !signed.wasm.is_empty() {
        let mut m = signed.clone();
        let i = pick % m.wasm.len();
        m.wasm[i] ^= 1 << (pick % 8);
        assert!(m.verify(&[trusted]).is_err(), "component bit flip still verifies");
    }
    // Appending to either part also breaks it (no length-extension ambiguity).
    let mut m = signed.clone();
    m.manifest.push(b' ');
    assert!(m.verify(&[trusted]).is_err(), "extended manifest still verifies");

    // The store round trip for packages that verify.
    let Some(v) = verified else {
        return;
    };
    let id = v.manifest.id.clone();
    let preview = fx
        .store
        .preview(&signed, &[trusted])
        .expect("a verified package previews on an empty store");
    assert!(preview.needs_approval && preview.installed_version.is_none());
    assert_eq!(preview.manifest, v.manifest);
    assert!(
        fx.store.install(&signed, &[trusted], &mut |_| false).is_err(),
        "installed without approval"
    );
    let installed = fx
        .store
        .install(&signed, &[trusted], &mut |_| true)
        .expect("a verified package installs");
    assert_eq!(installed.manifest, v.manifest);
    assert_eq!(installed.approved, v.manifest.capabilities);
    assert_eq!(installed.wasm_sha256, v.wasm_sha256);
    let loaded = fx.store.load_trusted(&id, &[trusted]).expect("installed plugin loads");
    assert_eq!(loaded.wasm, v.wasm);
    assert_eq!(loaded.installed.manifest, v.manifest);
    assert!(
        fx.store.load_trusted(&id, &[fx.other.verifying_key()]).is_err(),
        "loaded under an untrusted key"
    );
    assert!(fx.store.remove(&id).expect("remove"), "installed plugin was not listed");
    assert!(fx.store.list().expect("list").is_empty());
});
