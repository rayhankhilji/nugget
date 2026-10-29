mod common;

use common::{REV, entry, test_key};
use nugget_core::Error;
use nugget_core::capability::Tier;
use nugget_models::{
    DEFAULT_MANIFEST_JSON, DEFAULT_MANIFEST_SIG, Manifest, ModelEntry, SigningKey,
    default_manifest, sign_manifest, trusted_keys, verify_manifest,
};

fn bytes_of(models: Vec<ModelEntry>) -> Vec<u8> {
    serde_json::to_vec_pretty(&Manifest {
        version: 3,
        models,
        runtimes: vec![],
    })
    .unwrap()
}

fn sample() -> Vec<u8> {
    bytes_of(vec![entry(
        "small",
        &format!("https://example.org/m/resolve/{REV}/small.gguf"),
        b"hello",
    )])
}

#[test]
fn accepts_valid_signature() {
    let bytes = sample();
    let key = test_key();
    let sig = sign_manifest(&bytes, &key);
    let m = verify_manifest(&bytes, sig.as_bytes(), &[key.verifying_key()]).unwrap();
    assert_eq!(m.version, 3);
    assert_eq!(m.models[0].id, "small");
    assert_eq!(m.get("small").unwrap().size, 5);
}

#[test]
fn accepts_raw_signature_and_any_trusted_key() {
    let bytes = sample();
    let key = test_key();
    let other = SigningKey::from_bytes(&[9u8; 32]);
    let raw = hex::decode(sign_manifest(&bytes, &key)).unwrap();
    assert_eq!(raw.len(), 64);
    verify_manifest(&bytes, &raw, &[other.verifying_key(), key.verifying_key()]).unwrap();
}

#[test]
fn rejects_wrong_key() {
    let bytes = sample();
    let sig = sign_manifest(&bytes, &test_key());
    let other = SigningKey::from_bytes(&[9u8; 32]);
    let err = verify_manifest(&bytes, sig.as_bytes(), &[other.verifying_key()]).unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
}

#[test]
fn rejects_tampered_manifest() {
    let bytes = sample();
    let key = test_key();
    let sig = sign_manifest(&bytes, &key);
    // Change the size by one digit; still valid JSON, still a plausible manifest.
    let tampered = String::from_utf8(bytes.clone())
        .unwrap()
        .replace("\"size\": 5", "\"size\": 6")
        .into_bytes();
    assert_ne!(tampered, bytes);
    let err = verify_manifest(&tampered, sig.as_bytes(), &[key.verifying_key()]).unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    // Even whitespace changes are rejected: the signature covers exact bytes.
    let mut ws = bytes.clone();
    ws.push(b'\n');
    assert!(verify_manifest(&ws, sig.as_bytes(), &[key.verifying_key()]).is_err());
}

#[test]
fn rejects_malformed_signature_and_empty_keyring() {
    let bytes = sample();
    let key = test_key();
    for bad in [&b""[..], b"zz", b"not a signature at all"] {
        let err = verify_manifest(&bytes, bad, &[key.verifying_key()]).unwrap_err();
        assert!(matches!(err, Error::Integrity(_)), "{err}");
    }
    let sig = sign_manifest(&bytes, &key);
    assert!(matches!(
        verify_manifest(&bytes, sig.as_bytes(), &[]).unwrap_err(),
        Error::Integrity(_)
    ));
}

/// A correctly signed manifest is still rejected if an entry breaks the rules.
#[test]
fn rejects_signed_but_invalid_entries() {
    let base = entry(
        "m",
        &format!("https://example.org/m/resolve/{REV}/m.gguf"),
        b"x",
    );
    type Mutation = Box<dyn Fn(&mut ModelEntry)>;
    let cases: Vec<(&str, Mutation)> = vec![
        (
            "moving ref",
            Box::new(|e| {
                e.revision = "main".into();
                e.url = "https://example.org/m/resolve/main/m.gguf".into();
            }),
        ),
        (
            "url not pinned",
            Box::new(|e| e.url = "https://example.org/m/resolve/main/m.gguf".into()),
        ),
        (
            "plain http",
            Box::new(|e| e.url = format!("http://example.org/m/resolve/{REV}/m.gguf")),
        ),
        ("bad sha", Box::new(|e| e.sha256 = "ABC".into())),
        (
            "path traversal",
            Box::new(|e| e.file_name = "../evil".into()),
        ),
        ("bad id", Box::new(|e| e.id = "Has Space".into())),
        ("zero size", Box::new(|e| e.size = 0)),
        ("no tiers", Box::new(|e| e.tiers.clear())),
        (
            "rerank with other tiers",
            Box::new(|e| e.tiers = vec![Tier::Embedding, Tier::Rerank]),
        ),
        (
            "bad semver",
            Box::new(|e| e.min_app_version = "soon".into()),
        ),
        (
            "bad mirror",
            Box::new(|e| e.mirrors = vec!["ftp://example.org/x".into()]),
        ),
    ];
    let key = test_key();
    for (name, mutate) in cases {
        let mut e = base.clone();
        mutate(&mut e);
        let bytes = bytes_of(vec![e]);
        let sig = sign_manifest(&bytes, &key);
        let err = verify_manifest(&bytes, sig.as_bytes(), &[key.verifying_key()]).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{name}: {err}");
    }

    let bytes = bytes_of(vec![base.clone(), base]);
    let sig = sign_manifest(&bytes, &key);
    let err = verify_manifest(&bytes, sig.as_bytes(), &[key.verifying_key()]).unwrap_err();
    assert!(
        matches!(&err, Error::Invalid(m) if m.contains("duplicate")),
        "{err}"
    );
}

#[test]
fn shipped_manifest_verifies_with_embedded_dev_key() {
    let m = default_manifest().unwrap();
    assert_eq!(m.models.len(), 4);
    for e in &m.models {
        assert_eq!(e.revision.len(), 40, "{}", e.id);
        assert!(e.url.contains(&e.revision), "{}", e.id);
        assert!(e.url.ends_with(&e.file_name), "{}", e.id);
        assert!(e.url.starts_with("https://huggingface.co/"), "{}", e.id);
        assert!(e.file_name.ends_with(".gguf"));
        assert_eq!(e.license, "Apache-2.0");
    }
    for tier in [Tier::Triage, Tier::Synthesis, Tier::Embedding, Tier::Rerank] {
        assert!(m.models.iter().any(|e| e.tiers.contains(&tier)), "{tier:?}");
    }
    // The reranker serves the rerank tier alone.
    let rerank = m
        .get("bge-reranker-v2-m3-q4km")
        .expect("the shipped reranker");
    assert_eq!(rerank.tiers, vec![Tier::Rerank]);
    assert_eq!(rerank.family, "bert");
    for e in &m.models {
        if e.tiers.contains(&Tier::Rerank) {
            assert_eq!(e.tiers, vec![Tier::Rerank], "{}", e.id);
        }
    }
    // Shipped bytes must not have been re-encoded (e.g. CRLF on checkout).
    assert!(!DEFAULT_MANIFEST_JSON.contains(&b'\r'));
    // And a flipped byte breaks it.
    let mut t = DEFAULT_MANIFEST_JSON.to_vec();
    let i = t.iter().position(|&b| b == b'7').unwrap();
    t[i] = b'8';
    assert!(verify_manifest(&t, DEFAULT_MANIFEST_SIG, &trusted_keys().unwrap()).is_err());
}
