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
//!
//! Every request made here is reported as an [`Exchange`] (host and byte counts, including any
//! host a redirect led to), so the caller can put it in the egress ledger.

use std::time::Duration;

use base64::Engine as _;
use minisign_verify::{PublicKey, Signature};
use serde_json::Value;

/// Feeds are a few KiB; anything this big isn't one.
const MAX_FEED_BYTES: usize = 1 << 20;
/// A minisign signature, base64-wrapped, is about 450 bytes.
const MAX_SIG_BYTES: usize = 8 << 10;
const USER_AGENT: &str = nugget_core::identity::USER_AGENT;

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
    /// The requests that fetched the feed itself (not its `.sig`), redirects included: the
    /// plugin's own check repeats them. Empty when the feed didn't come from [`fetch_verified`].
    pub fetched_by: Vec<Exchange>,
}

/// One HTTP request, for the egress ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    pub host: String,
    /// The request line and typical headers: an estimate, as for model downloads.
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl Exchange {
    /// A request to `url` about to be sent, nothing received yet.
    pub fn request(url: &reqwest::Url) -> Self {
        Self {
            host: host_of(url),
            bytes_sent: url.as_str().len() as u64 + 160,
            bytes_received: 0,
        }
    }
}

/// The host a URL names (no port: the ledger records hosts), or `unknown`.
pub fn host_of(url: &reqwest::Url) -> String {
    url.host_str().unwrap_or("unknown").to_owned()
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
    Ok(VerifiedFeed {
        json,
        version,
        fetched_by: Vec::new(),
    })
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

/// GET `url` into memory, refusing a non-success status or a body over `limit` bytes. The
/// request is added to `log` before it is sent, and a host a redirect led to gets its own entry.
async fn get(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
    log: &mut Vec<Exchange>,
) -> Result<Vec<u8>, FeedError> {
    let fetch = |e: reqwest::Error| FeedError::Fetch(e.without_url().to_string());
    let parsed = reqwest::Url::parse(url).map_err(|e| FeedError::Fetch(format!("{url}: {e}")))?;
    log.push(Exchange::request(&parsed));
    let mut resp = client.get(parsed.clone()).send().await.map_err(fetch)?;
    if host_of(resp.url()) != host_of(&parsed) {
        log.push(Exchange::request(resp.url()));
    }
    let entry = log.len() - 1;
    let status = resp.status();
    if !status.is_success() {
        return Err(FeedError::Fetch(format!("{url}: HTTP {status}")));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(fetch)? {
        log[entry].bytes_received += chunk.len() as u64;
        if body.len() + chunk.len() > limit {
            return Err(FeedError::Fetch(format!(
                "{url}: larger than {limit} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Download the feed at `url` and its `.sig`, and verify it (see [`verify`]). Every request
/// made, failed ones included, is added to `log`.
pub async fn fetch_verified(
    url: &str,
    pubkey_b64: &str,
    timeout: Duration,
    log: &mut Vec<Exchange>,
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
    let start = log.len();
    let feed = get(&client, url, MAX_FEED_BYTES, log).await?;
    let fetched_by = log[start..].to_vec();
    let sig = get(&client, &sig_url(url), MAX_SIG_BYTES, log).await?;
    Ok(VerifiedFeed {
        fetched_by,
        ..verify(&feed, &sig, pubkey_b64)?
    })
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

    /// A loopback HTTP server: `/redirect` sends to `/body` on `localhost` (another host name for
    /// the ledger), `/body` answers five bytes, anything else 404. Returns its port.
    async fn serve() -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let mut got = 0;
                    while !buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut buf[got..]).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => got += n,
                        }
                    }
                    let head = String::from_utf8_lossy(&buf[..got]).into_owned();
                    let reply = if head.starts_with("GET /redirect ") {
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}/body\r\nContent-Length: 0\r\n\r\n"
                        )
                    } else if head.starts_with("GET /body ") {
                        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_owned()
                    } else {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_owned()
                    };
                    let _ = sock.write_all(reply.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        port
    }

    fn client() -> reqwest::Client {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[tokio::test]
    async fn every_request_is_logged_with_its_hosts_and_bytes() {
        let port = serve().await;
        let client = client();
        let mut log = Vec::new();

        let url = format!("http://127.0.0.1:{port}/body");
        assert_eq!(get(&client, &url, 100, &mut log).await.unwrap(), b"hello");
        assert_eq!(log.len(), 1);
        assert_eq!(
            (log[0].host.as_str(), log[0].bytes_received),
            ("127.0.0.1", 5)
        );
        assert_eq!(log[0].bytes_sent, url.len() as u64 + 160);

        // A redirect to another host: both are in the log, the body counted where it came from.
        log.clear();
        let url = format!("http://127.0.0.1:{port}/redirect");
        assert_eq!(get(&client, &url, 100, &mut log).await.unwrap(), b"hello");
        let hosts: Vec<_> = log
            .iter()
            .map(|e| (e.host.as_str(), e.bytes_received))
            .collect();
        assert_eq!(hosts, [("127.0.0.1", 0), ("localhost", 5)]);

        // Failed requests went out too.
        log.clear();
        let url = format!("http://127.0.0.1:{port}/missing");
        assert!(matches!(
            get(&client, &url, 100, &mut log).await,
            Err(FeedError::Fetch(_))
        ));
        assert_eq!(log.len(), 1);
        let url = format!("http://127.0.0.1:{port}/body");
        assert!(get(&client, &url, 2, &mut log).await.is_err());
        assert_eq!(log.len(), 2);
    }
}
