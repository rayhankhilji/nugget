//! Manifest rollback protection and trusted-key selection. No network.

mod common;

use common::{REV, entry};
use nugget_core::Error;
use nugget_core::paths::Paths;
use nugget_models::{
    DEFAULT_MANIFEST_JSON, DEFAULT_MANIFEST_SIG, DEV_PUBLIC_KEY_HEX, MANIFEST_TRUST_FILE, Manifest,
    ManifestTrust, SigningKey, default_manifest_checked, parse_verifying_key, select_trusted_keys,
    sign_manifest, trusted_keys, verify_manifest, verify_manifest_checked,
    verify_manifest_with_key,
};

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn manifest_bytes(version: u64) -> Vec<u8> {
    serde_json::to_vec_pretty(&Manifest {
        version,
        models: vec![entry(
            "small",
            &format!("https://example.org/m/resolve/{REV}/small.gguf"),
            b"hello",
        )],
        runtimes: vec![],
    })
    .unwrap()
}

/// `(bytes, hex signature)` of a manifest at `version` signed by `k`.
fn signed(version: u64, k: &SigningKey) -> (Vec<u8>, String) {
    let b = manifest_bytes(version);
    let s = sign_manifest(&b, k);
    (b, s)
}

fn accept(trust: &ManifestTrust, version: u64, k: &SigningKey) -> nugget_core::Result<Manifest> {
    let (b, s) = signed(version, k);
    verify_manifest_checked(&b, s.as_bytes(), &[k.verifying_key()], trust)
}

fn paths_in(dir: &std::path::Path) -> Paths {
    Paths {
        data: dir.join("data"),
        config: dir.join("config"),
        cache: dir.join("cache"),
    }
}

#[test]
fn fresh_install_accepts_and_records() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("models");
    let trust = ManifestTrust::open(&root).unwrap();
    assert!(!trust.path().exists());
    let k = key(1);
    assert_eq!(trust.accepted_version(&k.verifying_key()).unwrap(), None);

    assert_eq!(accept(&trust, 4, &k).unwrap().version, 4);
    assert_eq!(trust.path(), root.join(MANIFEST_TRUST_FILE));
    assert!(trust.path().exists(), "state written, store dir created");
    assert_eq!(trust.accepted_version(&k.verifying_key()).unwrap(), Some(4));
}

#[test]
fn downgrade_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let trust = ManifestTrust::open(dir.path()).unwrap();
    let k = key(1);
    accept(&trust, 5, &k).unwrap();

    let err = accept(&trust, 3, &k).unwrap_err();
    let msg = err.to_string();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert!(msg.contains("version 3 is older than version 5"), "{msg}");
    assert!(msg.contains("rollback"), "{msg}");
    let prefix = &hex::encode(k.verifying_key().to_bytes())[..16];
    assert!(msg.contains(prefix), "{msg}");
    // The refusal does not lower the record.
    assert_eq!(trust.accepted_version(&k.verifying_key()).unwrap(), Some(5));

    // Also refused after a restart.
    let reopened = ManifestTrust::open(dir.path()).unwrap();
    assert!(accept(&reopened, 4, &k).is_err());
}

#[test]
fn equal_version_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let trust = ManifestTrust::open(dir.path()).unwrap();
    let k = key(1);
    accept(&trust, 5, &k).unwrap();
    accept(&trust, 5, &k).unwrap();
    let reopened = ManifestTrust::open(dir.path()).unwrap();
    accept(&reopened, 5, &k).unwrap();
    assert_eq!(
        reopened.accepted_version(&k.verifying_key()).unwrap(),
        Some(5)
    );
}

#[test]
fn upgrade_is_accepted_and_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let k = key(1);
    {
        let trust = ManifestTrust::open(dir.path()).unwrap();
        accept(&trust, 5, &k).unwrap();
        accept(&trust, 9, &k).unwrap();
    }
    let reopened = ManifestTrust::open(dir.path()).unwrap();
    assert_eq!(
        reopened.accepted_version(&k.verifying_key()).unwrap(),
        Some(9)
    );
    assert!(accept(&reopened, 8, &k).is_err());
    // No temporary files are left behind.
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from(MANIFEST_TRUST_FILE)]);
}

#[test]
fn versions_are_tracked_per_key() {
    let dir = tempfile::tempdir().unwrap();
    let trust = ManifestTrust::open(dir.path()).unwrap();
    let (a, b) = (key(1), key(2));
    accept(&trust, 10, &a).unwrap();
    // Key b's series starts independently, below a's serial.
    accept(&trust, 2, &b).unwrap();
    accept(&trust, 3, &b).unwrap();
    assert!(accept(&trust, 2, &b).is_err());
    assert!(accept(&trust, 9, &a).is_err());
    assert_eq!(
        trust.accepted_version(&a.verifying_key()).unwrap(),
        Some(10)
    );
    assert_eq!(trust.accepted_version(&b.verifying_key()).unwrap(), Some(3));
}

#[test]
fn the_verifying_key_is_the_one_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let trust = ManifestTrust::open(dir.path()).unwrap();
    let (a, b) = (key(1), key(2));
    let (bytes, sig) = signed(7, &b);
    let (m, used) = verify_manifest_with_key(
        &bytes,
        sig.as_bytes(),
        &[a.verifying_key(), b.verifying_key()],
    )
    .unwrap();
    assert_eq!((m.version, used), (7, b.verifying_key()));
    verify_manifest_checked(
        &bytes,
        sig.as_bytes(),
        &[a.verifying_key(), b.verifying_key()],
        &trust,
    )
    .unwrap();
    assert_eq!(trust.accepted_version(&a.verifying_key()).unwrap(), None);
    assert_eq!(trust.accepted_version(&b.verifying_key()).unwrap(), Some(7));
}

#[test]
fn a_bad_signature_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let trust = ManifestTrust::open(dir.path()).unwrap();
    let (bytes, sig) = signed(7, &key(1));
    let err = verify_manifest_checked(&bytes, sig.as_bytes(), &[key(2).verifying_key()], &trust)
        .unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert!(!trust.path().exists());
}

#[test]
fn corrupt_state_fails_closed() {
    let k = key(1);
    for body in [
        &b"{ not json"[..],
        br#"{"format":1,"keys":{"zz":3}}"#,
        br#"{"format":99,"keys":{}}"#,
        br#"{"format":1,"keys":{},"extra":true}"#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MANIFEST_TRUST_FILE);
        std::fs::write(&path, body).unwrap();
        let err = ManifestTrust::open(dir.path()).unwrap_err();
        assert!(matches!(err, Error::Integrity(_)), "{err}");
        assert!(err.to_string().contains("corrupt"), "{err}");

        // Corrupted after opening: checks fail closed too, and the file is left untouched.
        std::fs::remove_file(&path).unwrap();
        let trust = ManifestTrust::open(dir.path()).unwrap();
        std::fs::write(&path, body).unwrap();
        assert!(accept(&trust, 1, &k).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), body);
    }
}

#[test]
fn embedded_manifest_goes_through_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let m = default_manifest_checked(&paths).expect("shipped manifest verifies in tests");
    let trust = ManifestTrust::for_paths(&paths).unwrap();
    assert_eq!(trust.path(), paths.models_dir().join(MANIFEST_TRUST_FILE));

    let (_, used) = verify_manifest_with_key(
        DEFAULT_MANIFEST_JSON,
        DEFAULT_MANIFEST_SIG,
        &trusted_keys().unwrap(),
    )
    .unwrap();
    assert_eq!(trust.accepted_version(&used).unwrap(), Some(m.version));
    // Loading it again (every daemon start) is fine.
    default_manifest_checked(&paths).unwrap();

    // Once a newer manifest from the same key has been accepted, the bundled one is a rollback.
    std::fs::write(
        trust.path(),
        format!(
            r#"{{"format":1,"keys":{{"{}":{}}}}}"#,
            hex::encode(used.to_bytes()),
            m.version + 1
        ),
    )
    .unwrap();
    let err = default_manifest_checked(&paths).unwrap_err();
    assert!(err.to_string().contains("refusing a rollback"), "{err}");
}

#[test]
fn signature_files_may_carry_several_signatures() {
    let (dev, rel) = (key(1), key(2));
    let (bytes, s1) = signed(3, &dev);
    let s2 = sign_manifest(&bytes, &rel);
    let both = format!("# dev\n{s1}\n\n{s2}\n");
    for keys in [
        vec![dev.verifying_key()],
        vec![rel.verifying_key()],
        vec![rel.verifying_key(), dev.verifying_key()],
    ] {
        let (_, used) = verify_manifest_with_key(&bytes, both.as_bytes(), &keys).unwrap();
        assert_eq!(used, keys[0]);
    }
    assert!(verify_manifest(&bytes, both.as_bytes(), &[key(3).verifying_key()]).is_err());
    // A malformed line poisons the file.
    let bad = format!("{s1}\nnot-hex\n");
    assert!(verify_manifest(&bytes, bad.as_bytes(), &[dev.verifying_key()]).is_err());
    assert!(verify_manifest(&bytes, b"\n# nothing\n", &[dev.verifying_key()]).is_err());
}

#[test]
fn key_selection_release_and_dev() {
    let dev = parse_verifying_key(DEV_PUBLIC_KEY_HEX).unwrap();
    let rel = key(7).verifying_key();
    let rel_hex = hex::encode(rel.to_bytes());

    assert_eq!(select_trusted_keys(None, true).unwrap(), vec![dev]);
    assert_eq!(select_trusted_keys(Some(""), true).unwrap(), vec![dev]);
    assert_eq!(
        select_trusted_keys(Some(&rel_hex), false).unwrap(),
        vec![rel]
    );
    assert_eq!(
        select_trusted_keys(Some(&rel_hex), true).unwrap(),
        vec![rel, dev],
        "release key first"
    );
    // The dev key given as the release key is not listed twice.
    assert_eq!(
        select_trusted_keys(Some(DEV_PUBLIC_KEY_HEX), true).unwrap(),
        vec![dev]
    );

    // A release build without a release key or the opt-in trusts nothing, loudly.
    let err = select_trusted_keys(None, false).unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert!(
        err.to_string().contains("NUGGET_MANIFEST_RELEASE_PUBKEY"),
        "{err}"
    );
    assert!(
        err.to_string().contains("NUGGET_TRUST_DEV_MANIFEST_KEY"),
        "{err}"
    );

    let err = select_trusted_keys(Some("abcd"), true).unwrap_err();
    assert!(
        err.to_string().contains("NUGGET_MANIFEST_RELEASE_PUBKEY"),
        "{err}"
    );
}

#[test]
fn release_only_keys_reject_the_dev_signed_manifest() {
    let rel = hex::encode(key(7).verifying_key().to_bytes());
    let keys = select_trusted_keys(Some(&rel), false).unwrap();
    assert!(verify_manifest(DEFAULT_MANIFEST_JSON, DEFAULT_MANIFEST_SIG, &keys).is_err());
    let keys = select_trusted_keys(Some(&rel), true).unwrap();
    verify_manifest(DEFAULT_MANIFEST_JSON, DEFAULT_MANIFEST_SIG, &keys).unwrap();
}

#[cfg(debug_assertions)]
#[test]
fn debug_builds_trust_the_dev_key() {
    assert!(nugget_models::dev_key_trusted());
    let dev = parse_verifying_key(DEV_PUBLIC_KEY_HEX).unwrap();
    assert!(trusted_keys().unwrap().contains(&dev));
}
