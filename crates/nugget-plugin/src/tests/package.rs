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

const MAGIC: &[u8] = b"NUGGETPKG\x01";

fn section(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

#[test]
fn single_file_round_trip() {
    let k = key(1);
    let pkg = package(&manifest(), EMPTY_COMPONENT, &k);
    let bytes = pkg.to_bytes();
    assert!(bytes.starts_with(MAGIC));
    let mut expected = MAGIC.to_vec();
    section(&mut expected, &pkg.manifest);
    section(&mut expected, &pkg.wasm);
    section(&mut expected, &pkg.signature);
    assert_eq!(bytes, expected);
    let back = PluginPackage::from_bytes(&bytes).unwrap();
    assert_eq!(back.manifest, pkg.manifest);
    assert_eq!(back.wasm, pkg.wasm);
    assert_eq!(back.signature, pkg.signature);
    back.verify(&[k.verifying_key()]).unwrap();

    // An unsigned package (empty signature section) round-trips but does not verify.
    let unsigned = PluginPackage {
        signature: Vec::new(),
        ..pkg
    };
    let back = PluginPackage::from_bytes(&unsigned.to_bytes()).unwrap();
    assert!(back.signature.is_empty());
    assert!(back.verify(&[k.verifying_key()]).is_err());
}

#[test]
fn malformed_single_files_are_rejected() {
    let bytes = package(&manifest(), EMPTY_COMPONENT, &key(1)).to_bytes();
    let invalid = |b: &[u8]| {
        matches!(
            PluginPackage::from_bytes(b),
            Err(nugget_core::Error::Invalid(_))
        )
    };

    // Every truncation, including inside a length prefix and the empty input.
    for n in 0..bytes.len() {
        assert!(invalid(&bytes[..n]), "truncated to {n} bytes");
    }
    // Trailing bytes.
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(invalid(&trailing));
    // Bad magic and an unknown format version.
    let mut magic = bytes.clone();
    magic[0] = b'X';
    assert!(invalid(&magic));
    let mut version = bytes.clone();
    version[9] = 2;
    assert!(invalid(&version));

    // Oversized sections are refused from their length prefix alone.
    let mut big_manifest = MAGIC.to_vec();
    big_manifest.extend_from_slice(&(crate::manifest::MAX_MANIFEST_BYTES as u32 + 1).to_le_bytes());
    assert!(invalid(&big_manifest));
    let mut big_wasm = MAGIC.to_vec();
    section(&mut big_wasm, b"m");
    big_wasm.extend_from_slice(&(crate::MAX_WASM_BYTES as u32 + 1).to_le_bytes());
    assert!(invalid(&big_wasm));
    let mut big_sig = MAGIC.to_vec();
    section(&mut big_sig, b"m");
    section(&mut big_sig, b"w");
    section(&mut big_sig, &[b'a'; 1025]);
    assert!(invalid(&big_sig));
    let mut max_sig = MAGIC.to_vec();
    section(&mut max_sig, b"m");
    section(&mut max_sig, b"w");
    section(&mut max_sig, &[b'a'; 1024]);
    assert!(PluginPackage::from_bytes(&max_sig).is_ok());
}

#[test]
fn from_path_reads_directories_and_files() {
    let k = key(1);
    let trusted = [k.verifying_key()];
    let pkg = package(&manifest(), EMPTY_COMPONENT, &k);
    let tmp = tempfile::tempdir().unwrap();

    let dir = tmp.path().join("pkg");
    pkg.write_dir(&dir).unwrap();
    PluginPackage::from_path(&dir)
        .unwrap()
        .verify(&trusted)
        .unwrap();

    let file = tmp
        .path()
        .join("nested")
        .join(format!("test.{}", crate::PACKAGE_EXTENSION));
    pkg.write_file(&file).unwrap();
    let back = PluginPackage::from_path(&file).unwrap();
    assert_eq!(back.manifest, pkg.manifest);
    back.verify(&trusted).unwrap();
    assert_eq!(
        PluginPackage::from_file(&file).unwrap().to_bytes(),
        pkg.to_bytes()
    );

    let junk = tmp.path().join("junk.nuggetplugin");
    std::fs::write(&junk, b"not a package").unwrap();
    assert!(PluginPackage::from_path(&junk).is_err());
    assert!(PluginPackage::from_path(&tmp.path().join("missing")).is_err());
}
