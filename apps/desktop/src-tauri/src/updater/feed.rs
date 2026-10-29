//! The signed update feed.
//!
//! A channel feed (`stable.json`, `beta.json`) is the Tauri updater manifest: version, notes and,
//! per platform, a payload URL and that payload's minisign signature. The release workflow signs
//! the feed itself too, with the same updater key, and publishes the signature next to it as
//! `<feed>.sig` (the base64 text `tauri signer sign` writes, as for payloads).
//!
//! `tauri-plugin-updater` fetches and parses the feed on its own and can't be handed bytes, so a
//! check runs in two steps:
//!
//! 1. [`fetch_verified`] downloads the feed and its `.sig` and verifies the signature over the
//!    exact bytes against the key compiled into this build. Nothing is parsed before that, and
//!    on failure the plugin is never called.
//! 2. The plugin then runs its own check against the same URL. [`consistent`] accepts its answer
//!    only if it read the very feed that was verified: the same version and, when it offers an
//!    update, the same JSON document. A feed that changed between the two requests (a move by
//!    the release workflow, or tampering) fails the check, and the next check tries again.

use std::time::Duration;

use base64::Engine as _;
use minisign_verify::{PublicKey, Signature};
use serde_json::Value;

/// Feeds are a few KiB; anything this big isn't one.
const MAX_FEED_BYTES: usize = 1 << 20;
/// A minisign signature, base64-wrapped, is about 450 bytes.
const MAX_SIG_BYTES: usize = 8 << 10;
const USER_AGENT: &str = concat!("nugget-desktop/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FeedError {
    #[error("couldn't fetch the update feed: {0}")]
    Fetch(String),
    #[error("the update key compiled into this build is invalid: {0}")]
    Key(String),
    #[error("the update feed's signature is missing or malformed: {0}")]
    BadSignature(String),
    #[error("the update feed failed signature verification: {0}")]
    Verify(String),
    #[error("the signed update feed is not a valid manifest: {0}")]
    Manifest(String),
    #[error("the update feed changed while it was being checked; try again")]
    Changed,
}

/// A feed whose signature verified, parsed only afterwards.
#[derive(Debug, Clone)]
pub struct VerifiedFeed {
    pub json: Value,
    /// `version` from the feed, without a leading `v`.
    pub version: String,
}

/// Where a feed's detached signature lives.
pub fn sig_url(feed_url: &str) -> String {
    format!("{feed_url}.sig")
}

fn decode_b64_text(b64: &[u8]) -> Result<String, String> {
    let trimmed = b64.trim_ascii();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .map_err(|e| format!("not base64: {e}"))?;
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_owned())
}

/// Verify `sig_file` (the `.sig` contents: base64 of a minisign signature) over the exact `feed`
/// bytes with `pubkey_b64` (base64 of a minisign public key, as `NUGGET_UPDATER_PUBKEY` holds),
/// and only then parse the feed.
pub fn verify(feed: &[u8], sig_file: &[u8], pubkey_b64: &str) -> Result<VerifiedFeed, FeedError> {
    let key_text = decode_b64_text(pubkey_b64.as_bytes()).map_err(FeedError::Key)?;
    let key = PublicKey::decode(&key_text).map_err(|e| FeedError::Key(e.to_string()))?;
    if feed.len() > MAX_FEED_BYTES {
        return Err(FeedError::Manifest(format!("{} bytes", feed.len())));
    }
    if sig_file.is_empty() || sig_file.len() > MAX_SIG_BYTES {
        return Err(FeedError::BadSignature(format!("{} bytes", sig_file.len())));
    }
    let sig_text = decode_b64_text(sig_file).map_err(FeedError::BadSignature)?;
    let sig = Signature::decode(&sig_text).map_err(|e| FeedError::BadSignature(e.to_string()))?;
    // `true` accepts minisign's legacy (non-prehashed) form as well, like the plugin does for
    // payloads; both are Ed25519 signatures by the same key over these bytes.
    key.verify(feed, &sig, true)
        .map_err(|e| FeedError::Verify(e.to_string()))?;

    let json: Value =
        serde_json::from_slice(feed).map_err(|e| FeedError::Manifest(e.to_string()))?;
    let version = json
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| FeedError::Manifest("no version".to_owned()))?
        .trim_start_matches('v')
        .to_owned();
    Ok(VerifiedFeed { json, version })
}

/// Did the plugin act on the feed that was verified? `seen_version` is the version the plugin
/// parsed (its version comparator records it; `None` if it never got that far), `offered` the
/// raw JSON behind the update it returned, if any.
pub fn consistent(
    verified: &VerifiedFeed,
    seen_version: Option<&str>,
    offered: Option<&Value>,
) -> Result<(), FeedError> {
    if seen_version.map(|v| v.trim_start_matches('v')) != Some(verified.version.as_str()) {
        return Err(FeedError::Changed);
    }
    match offered {
        Some(json) if *json != verified.json => Err(FeedError::Changed),
        _ => Ok(()),
    }
}

/// GET `url` into memory, refusing a non-success status or a body over `limit` bytes.
async fn get(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, FeedError> {
    let fetch = |e: reqwest::Error| FeedError::Fetch(e.without_url().to_string());
    let mut resp = client.get(url).send().await.map_err(fetch)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(FeedError::Fetch(format!("{url}: HTTP {status}")));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(fetch)? {
        if body.len() + chunk.len() > limit {
            return Err(FeedError::Fetch(format!(
                "{url}: larger than {limit} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Download the feed at `url` and its `.sig`, and verify it (see [`verify`]).
pub async fn fetch_verified(
    url: &str,
    pubkey_b64: &str,
    timeout: Duration,
) -> Result<VerifiedFeed, FeedError> {
    // Same TLS setup as the updater plugin: rustls with the `ring` provider, installed as the
    // process default if nothing else has been (the plugin does exactly this before its request).
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()
        .map_err(|e| FeedError::Fetch(e.to_string()))?;
    let feed = get(&client, url, MAX_FEED_BYTES).await?;
    let sig = get(&client, &sig_url(url), MAX_SIG_BYTES).await?;
    verify(&feed, &sig, pubkey_b64)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures: minisign keys and signatures in the format `tauri signer` produces (prehashed
    // Ed25519, base64-wrapped text), made offline for these tests only. Key A is the "build"
    // key. B is another key; `SAME_ID_OTHER_KEY_SIG` is B's signature relabelled with A's key
    // id, so it reaches the Ed25519 check instead of failing on the id.
    const TEST_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDA4MDcwNjA1MDQwMzAyMDEKUldRQkFnTUVCUVlIQ0R0MkphU2VQZlVxaE1ZM2Eyb2svbkVqZTNzME1BM3ZFM1lEdk5yTW12ZW4K";
    const FEED: &str = "{\n  \"version\": \"9.9.9\",\n  \"notes\": \"test\",\n  \"pub_date\": \"2026-10-29T00:00:00Z\",\n  \"platforms\": {\n    \"linux-x86_64\": {\n      \"signature\": \"c2ln\",\n      \"url\": \"https://example.invalid/Nugget_9.9.9_amd64.AppImage\"\n    }\n  }\n}\n";
    const FEED_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRQkFnTUVCUVlIQ0ZDZmUwZE96Q0pXNzhrM2lZYmlDbkFIeDZHamdQZ2FwMVVuVVRXMHk1aGdMOE5qQXE1Vkx2K3J3Q0hUUTh4alo1K2tKM1UzcnpDdzE3N1c5R3pXUndFPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkzMjMyMDAwCWZpbGU6bGF0ZXN0Lmpzb24KRzkrZUZUaDJoRzVKWmM2blJNOGxEbmNELzIvbnYzVEtuTSs5alh6TFRkRlY3L0E1emMzNE1Cdi9YWjJXNHRqVFg0b2luZVorNkFFd0dIcnJyU2FmRFE9PQo=";
    const OTHER_KEY_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRUkVoTVVGUllYR09sNVVtQjA0ajZoKzRINFFBQ0tLTmtDbHVxRS9yN01BeDBoaEFwYlNtQkVpNEM4QVFxeG5qZWY3Rm9pZ1hiREpRT1BqR2JmZ2M3K1M1bXlVb3ZTTHdVPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkzMjMyMDAwCWZpbGU6bGF0ZXN0Lmpzb24Kc2ZrWDVUd2dCNkJCT3FmTnN3THBxOGx3aEhjOVZLaXo4SnRiZGJseWtrUzBIcUsvNjNtVEptc21qeUFDWnVOYlJOdlV5TEtuc3hVWm5CZDVzUGcxRFE9PQo=";
    const SAME_ID_OTHER_KEY_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRQkFnTUVCUVlIQ09sNVVtQjA0ajZoKzRINFFBQ0tLTmtDbHVxRS9yN01BeDBoaEFwYlNtQkVpNEM4QVFxeG5qZWY3Rm9pZ1hiREpRT1BqR2JmZ2M3K1M1bXlVb3ZTTHdVPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkzMjMyMDAwCWZpbGU6bGF0ZXN0Lmpzb24Kc2ZrWDVUd2dCNkJCT3FmTnN3THBxOGx3aEhjOVZLaXo4SnRiZGJseWtrUzBIcUsvNjNtVEptc21qeUFDWnVOYlJOdlV5TEtuc3hVWm5CZDVzUGcxRFE9PQo=";
    /// Key A's signature over `b"not json\n"`.
    const NOT_JSON_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRQkFnTUVCUVlIQ051YWV0V0JVMFE2SDM5OWVuemQ2bDYwOTdtQWoyUUpTUy96RnpVbVdLc09teTh3clN3WmpldlQrQ2hsV3NtZFpXKyt1bE14cjU0UVZRc0pYWlpHSndvPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkzMjMyMDAwCWZpbGU6bGF0ZXN0Lmpzb24KWGVleDRJOTJhU3piZjJ0MTBKeE1mTHlCTnNnQ29lU1VvTGo5RXdlQVpxL05xNXBWWm5qU1Y5VnZLeDBGSnpLNkF0NDVKRzdYNDhnaVhhTmFqR1RpQUE9PQo=";

    fn ok() -> VerifiedFeed {
        verify(FEED.as_bytes(), FEED_SIG.as_bytes(), TEST_PUBKEY).unwrap()
    }

    #[test]
    fn valid_feed_verifies_then_parses() {
        let feed = ok();
        assert_eq!(feed.version, "9.9.9");
        assert_eq!(feed.json["platforms"]["linux-x86_64"]["signature"], "c2ln");
        // The .sig file as downloaded may end in a newline.
        let with_newline = format!("{FEED_SIG}\n");
        assert!(verify(FEED.as_bytes(), with_newline.as_bytes(), TEST_PUBKEY).is_ok());
    }

    #[test]
    fn any_changed_byte_is_rejected() {
        let original = FEED.as_bytes();
        // Every position, so the version, a URL and the payload signature are all covered.
        for i in 0..original.len() {
            let mut tampered = original.to_vec();
            tampered[i] ^= 0x01;
            let e = verify(&tampered, FEED_SIG.as_bytes(), TEST_PUBKEY).unwrap_err();
            assert!(matches!(e, FeedError::Verify(_)), "byte {i}: {e}");
        }
        let mut appended = original.to_vec();
        appended.push(b'\n');
        assert!(matches!(
            verify(&appended, FEED_SIG.as_bytes(), TEST_PUBKEY),
            Err(FeedError::Verify(_))
        ));
        assert!(matches!(
            verify(
                &original[..original.len() - 1],
                FEED_SIG.as_bytes(),
                TEST_PUBKEY
            ),
            Err(FeedError::Verify(_))
        ));
    }

    #[test]
    fn missing_or_garbage_signature_is_rejected() {
        let garbage_text = base64::engine::general_purpose::STANDARD.encode("hello\nworld\n");
        let truncated = &FEED_SIG[..FEED_SIG.len() / 2];
        for sig in [
            "",
            "   \n",
            "not base64 at all!",
            garbage_text.as_str(),
            truncated,
            // A payload-style raw minisign text, not base64-wrapped.
            "untrusted comment: x\nRUQ=\ntrusted comment: y\nAA==\n",
        ] {
            let e = verify(FEED.as_bytes(), sig.as_bytes(), TEST_PUBKEY).unwrap_err();
            assert!(matches!(e, FeedError::BadSignature(_)), "{sig:?}: {e}");
        }
        let huge = vec![b'A'; MAX_SIG_BYTES + 1];
        assert!(matches!(
            verify(FEED.as_bytes(), &huge, TEST_PUBKEY),
            Err(FeedError::BadSignature(_))
        ));
    }

    #[test]
    fn signature_from_another_key_is_rejected() {
        for sig in [OTHER_KEY_SIG, SAME_ID_OTHER_KEY_SIG] {
            let e = verify(FEED.as_bytes(), sig.as_bytes(), TEST_PUBKEY).unwrap_err();
            assert!(matches!(e, FeedError::Verify(_)), "{e}");
        }
    }

    #[test]
    fn signature_for_another_file_is_rejected() {
        // A valid signature by the right key, but over different bytes.
        assert!(matches!(
            verify(FEED.as_bytes(), NOT_JSON_SIG.as_bytes(), TEST_PUBKEY),
            Err(FeedError::Verify(_))
        ));
    }

    #[test]
    fn signed_but_not_a_manifest_is_rejected() {
        assert!(matches!(
            verify(b"not json\n", NOT_JSON_SIG.as_bytes(), TEST_PUBKEY),
            Err(FeedError::Manifest(_))
        ));
    }

    #[test]
    fn bad_build_key_is_reported_as_such() {
        for key in ["", "garbage", "aGVsbG8="] {
            assert!(matches!(
                verify(FEED.as_bytes(), FEED_SIG.as_bytes(), key),
                Err(FeedError::Key(_))
            ));
        }
    }

    #[test]
    fn oversized_feed_is_rejected() {
        let big = vec![b' '; MAX_FEED_BYTES + 1];
        assert!(verify(&big, FEED_SIG.as_bytes(), TEST_PUBKEY).is_err());
    }

    #[test]
    fn plugin_must_have_read_the_verified_feed() {
        let feed = ok();
        // No update offered, same version seen: fine (with or without a `v`).
        assert_eq!(consistent(&feed, Some("9.9.9"), None), Ok(()));
        assert_eq!(consistent(&feed, Some("v9.9.9"), None), Ok(()));
        // Update offered from the identical document: fine.
        assert_eq!(consistent(&feed, Some("9.9.9"), Some(&feed.json)), Ok(()));
        // The plugin saw another version, or never parsed a release.
        assert_eq!(
            consistent(&feed, Some("9.9.10"), None),
            Err(FeedError::Changed)
        );
        assert_eq!(consistent(&feed, None, None), Err(FeedError::Changed));
        // Same version, different document (e.g. a swapped payload URL).
        let mut other = feed.json.clone();
        other["platforms"]["linux-x86_64"]["url"] = "https://evil.invalid/x".into();
        assert_eq!(
            consistent(&feed, Some("9.9.9"), Some(&other)),
            Err(FeedError::Changed)
        );
    }

    #[test]
    fn signature_sits_next_to_the_feed() {
        assert_eq!(
            sig_url("https://example.invalid/feed/stable.json"),
            "https://example.invalid/feed/stable.json.sig"
        );
    }
}
