//! Signed model manifest: signature-file decoding, then JSON parsing and validation.
//!
//! The first byte picks the mode. Odd: the rest is a signature file checked against the bundled
//! manifest. Even: the rest is manifest JSON, signed here with a fixed test key so the parser and
//! validator behind the signature check are reached.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nugget_models::{DEFAULT_MANIFEST_JSON, SigningKey, semver, sign_manifest, verify_manifest};

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let key = SigningKey::from_bytes(&[7; 32]);
    let keys = [key.verifying_key()];
    if mode & 1 == 1 {
        let _ = verify_manifest(DEFAULT_MANIFEST_JSON, rest, &keys);
        return;
    }
    let sig = sign_manifest(rest, &key);
    let Ok(manifest) = verify_manifest(rest, sig.as_bytes(), &keys) else {
        return;
    };
    let app = semver::Version::new(1, 0, 0);
    for m in &manifest.models {
        assert!(manifest.get(&m.id).is_some());
        assert!(m.sources().count() >= 1);
        let _ = m.compatible_with(&app);
        // Every entry of an accepted manifest passes validation on its own.
        m.validate().expect("accepted entry must validate");
    }
});
