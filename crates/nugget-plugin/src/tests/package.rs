use super::{EMPTY_COMPONENT, key, manifest_toml, package};
use crate::{PluginPackage, parse_signing_key, parse_verifying_key, sign_package};

fn manifest() -> String {
    manifest_toml("dev.nugget.test", "1.0.0", "[\"tool\"]", "")
}

#[test]
fn a_signed_package_verifies() {
    let k = key(1);
    let pkg = package(&manifest(), EMPTY_COMPONENT, &k);
    let v = pkg.verify(&[k.verifying_key()]).unwrap();
    assert_eq!(v.manifest.id, "dev.nugget.test");
    assert_eq!(v.signer, k.verifying_key());
    // Raw 64-byte signatures are accepted too.
    let mut raw = pkg.clone();
    raw.signature = hex::decode(&pkg.signature).unwrap();
    raw.verify(&[k.verifying_key()]).unwrap();
}

#[test]
fn signature_mismatch_is_rejected() {
    let k = key(1);
    let pkg = package(&manifest(), EMPTY_COMPONENT, &k);
    let trusted = [k.verifying_key()];

    let mut wasm = pkg.clone();
    wasm.wasm.push(0);
    assert!(matches!(
        wasm.verify(&trusted),
        Err(nugget_core::Error::Integrity(_))
    ));

    // A widened capability in the manifest breaks the signature before it is even parsed.
    let mut man = pkg.clone();
    man.manifest = manifest_toml(
        "dev.nugget.test",
        "1.0.0",
        "[\"tool\"]",
        "hosts = [\"evil.com\"]",
    )
    .into_bytes();
    assert!(matches!(
        man.verify(&trusted),
        Err(nugget_core::Error::Integrity(_))
    ));

    assert!(
        pkg.verify(&[key(2).verifying_key()]).is_err(),
        "untrusted key"
    );
    assert!(pkg.verify(&[]).is_err(), "no trusted keys");
    let mut garbage = pkg.clone();
    garbage.signature = b"zz".to_vec();
    assert!(garbage.verify(&trusted).is_err());
}

#[test]
fn signatures_do_not_transfer_between_files() {
    // Swapping manifest and component bytes keeps the same pair of hashes but in other roles.
    let k = key(1);
    let m = manifest();
    let sig = sign_package(m.as_bytes(), EMPTY_COMPONENT, &k);
    let swapped = PluginPackage {
        manifest: EMPTY_COMPONENT.to_vec(),
        wasm: m.into_bytes(),
        signature: sig.into_bytes(),
    };
    assert!(swapped.verify(&[k.verifying_key()]).is_err());
}

#[test]
fn core_modules_are_not_components() {
    let k = key(1);
    let core_module = b"\0asm\x01\x00\x00\x00";
    let pkg = package(&manifest(), core_module, &k);
    assert!(pkg.verify(&[k.verifying_key()]).is_err());
}

#[test]
fn package_dir_round_trip_and_key_parsing() {
    let k = parse_signing_key(&"07".repeat(32)).unwrap();
    let vk = parse_verifying_key(&hex::encode(k.verifying_key().as_bytes())).unwrap();
    let dir = tempfile::tempdir().unwrap();
    package(&manifest(), EMPTY_COMPONENT, &k)
        .write_dir(dir.path())
        .unwrap();
    let pkg = PluginPackage::from_dir(dir.path()).unwrap();
    pkg.verify(&[vk]).unwrap();
    assert!(parse_verifying_key("abcd").is_err());
}
