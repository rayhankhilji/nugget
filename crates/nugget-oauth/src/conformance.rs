//! Conformance: token expiry and refresh, rotation, revocation, outages, clock skew and secret
//! hygiene against a mock token endpoint.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use nugget_connector::SecretStore;
use nugget_connector::http::MeteredHttp;
use nugget_connector::testkit::{MemSecrets, Sequence, VecEgress, server_error, too_many_requests};
use nugget_core::{Error, Result};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::*;

const ACCOUNT: &str = "oauth:gmail:conformance";

struct Env {
    server: MockServer,
    egress: Arc<VecEgress>,
    secrets: Arc<MemSecrets>,
    client: OAuthClient,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let egress = Arc::new(VecEgress::default());
    let http = MeteredHttp::new(egress.clone(), "sync:gmail").unwrap();
    let provider = google::provider_with_endpoints(
        &format!("{}/auth", server.uri()),
        &format!("{}/token", server.uri()),
        &[google::SCOPE_GMAIL_READONLY],
    );
    let creds = ClientCredentials::new("cid.apps.googleusercontent.com", Some("s3cret".into()));
    Env {
        server,
        egress,
        secrets: Arc::new(MemSecrets::default()),
        client: OAuthClient::new(provider, creds, http).unwrap(),
    }
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn seed(secrets: &dyn SecretStore, access: &str, refresh: Option<&str>, expires_at: Option<i64>) {
    let t = TokenSet {
        access_token: access.into(),
        refresh_token: refresh.map(Into::into),
        expires_at,
        token_type: Some("Bearer".into()),
        scope: None,
    };
    secrets
        .set(ACCOUNT, &serde_json::to_string(&t).unwrap())
        .unwrap();
}

fn stored(secrets: &dyn SecretStore) -> TokenSet {
    serde_json::from_str(&secrets.get(ACCOUNT).unwrap().unwrap()).unwrap()
}

fn ok(access: &str, refresh: Option<&str>, expires_in: serde_json::Value) -> ResponseTemplate {
    let mut v = serde_json::json!({"access_token": access, "token_type": "Bearer"});
    if !expires_in.is_null() {
        v["expires_in"] = expires_in;
    }
    if let Some(r) = refresh {
        v["refresh_token"] = r.into();
    }
    ResponseTemplate::new(200).set_body_json(v)
}

fn source(e: &Env) -> TokenSource {
    TokenSource::new(e.client.clone(), e.secrets.clone(), ACCOUNT)
}

#[tokio::test]
async fn token_about_to_expire_is_refreshed_early() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ok("at-new", None, 3600.into()))
        .expect(1)
        .mount(&e.server)
        .await;
    // Expires in 60 s by our clock: inside the 2 minute margin, so refreshed now rather than
    // racing the expiry mid-request (or losing to a server clock that runs ahead).
    seed(&*e.secrets, "at-old", Some("rt"), Some(now() + 60));
    let ts = source(&e);
    assert_eq!(ts.access_token().await.unwrap(), "at-new");
    let t = stored(&*e.secrets);
    assert!(t.expires_at.unwrap() >= now() + 3500);
    // A token that is already expired (local clock ahead) is refreshed too.
    seed(&*e.secrets, "at-x", Some("rt"), Some(now() - 600));
    let e2 = env().await;
    Mock::given(method("POST"))
        .respond_with(ok("at-y", None, 3600.into()))
        .expect(1)
        .mount(&e2.server)
        .await;
    seed(&*e2.secrets, "at-x", Some("rt"), Some(now() - 600));
    assert_eq!(source(&e2).access_token().await.unwrap(), "at-y");
}

#[tokio::test]
async fn concurrent_refreshes_coalesce_into_one_request() {
    let e = env().await;
    Mock::given(method("POST"))
        .respond_with(ok("at-new", Some("rt-2"), 3600.into()).set_delay(Duration::from_millis(50)))
        .expect(1)
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-old", Some("rt-1"), Some(now() - 1));
    let ts = Arc::new(source(&e));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let ts = ts.clone();
        tasks.push(tokio::spawn(async move { ts.access_token().await }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap(), "at-new");
    }
    // Concurrent 401s on the same rejected token also share one refresh (already done above).
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let ts = ts.clone();
        tasks.push(tokio::spawn(
            async move { ts.force_refresh("at-old").await },
        ));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap(), "at-new");
    }
    assert_eq!(e.egress.events().len(), 1);
}

#[tokio::test]
async fn rotated_refresh_token_is_persisted_and_used_next() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-1"))
        .respond_with(ok("at-2", Some("rt-2"), 1.into()))
        .expect(1)
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-2"))
        .respond_with(ok("at-3", Some("rt-3"), 3600.into()))
        .expect(1)
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-1", Some("rt-1"), Some(now() - 1));
    let ts = source(&e);
    assert_eq!(ts.access_token().await.unwrap(), "at-2");
    assert_eq!(stored(&*e.secrets).refresh_token.as_deref(), Some("rt-2"));
    // at-2 lives 1 s, inside the margin: the next call refreshes with the rotated token.
    assert_eq!(ts.access_token().await.unwrap(), "at-3");
    assert_eq!(stored(&*e.secrets).refresh_token.as_deref(), Some("rt-3"));
    // A fresh source (daemon restart) reads the rotated set from the keychain.
    assert_eq!(source(&e).access_token().await.unwrap(), "at-3");
}

#[tokio::test]
async fn revoked_refresh_token_asks_to_reconnect() {
    let e = env().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "Token has been expired or revoked."
        })))
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-old", Some("rt-revoked"), Some(now() - 10));
    let ts = source(&e);
    for _ in 0..2 {
        let err = ts.access_token().await.unwrap_err();
        assert!(
            matches!(err, Error::Unauthorized(ref m) if m.contains("reconnect")),
            "{err}"
        );
    }
    // The 401 path surfaces it the same way.
    let err = ts.force_refresh("at-old").await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized(ref m) if m.contains("reconnect")));
    // Nothing is deleted: the stored set stays for the user to replace by reconnecting.
    assert_eq!(stored(&*e.secrets).access_token, "at-old");
}

#[tokio::test]
async fn token_endpoint_outages_are_transient_not_unauthorized() {
    let e = env().await;
    for (rt, resp) in [
        ("rt-429", too_many_requests(7)),
        ("rt-500", server_error(500, None)),
        ("rt-502", server_error(502, None)),
        ("rt-503ra", server_error(503, Some(4))),
        (
            "rt-tu",
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({"error": "temporarily_unavailable"})),
        ),
        (
            "rt-html",
            ResponseTemplate::new(503).set_body_string("<html>maintenance</html>"),
        ),
    ] {
        Mock::given(method("POST"))
            .and(body_string_contains(format!("refresh_token={rt}&")))
            .respond_with(resp)
            .mount(&e.server)
            .await;
    }
    let err = e.client.refresh("rt-429").await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(7000)
            }
        ),
        "{err}"
    );
    let err = e.client.refresh("rt-503ra").await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(4000)
            }
        ),
        "{err}"
    );
    for rt in ["rt-500", "rt-502", "rt-tu", "rt-html"] {
        let err = e.client.refresh(rt).await.unwrap_err();
        assert!(matches!(err, Error::Network(_)), "{rt}: {err}");
    }
    // Through a TokenSource with an expired token: the error is transient, and the next call
    // after the outage succeeds without the user doing anything.
    let e = env().await;
    let seq = Arc::new(Sequence::new(vec![
        server_error(503, None),
        too_many_requests(1),
        ok("at-back", None, 3600.into()),
    ]));
    Mock::given(method("POST"))
        .respond_with(SeqRef(seq.clone()))
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-old", Some("rt"), Some(now() - 5));
    let ts = source(&e);
    assert!(matches!(ts.access_token().await, Err(Error::Network(_))));
    assert!(matches!(
        ts.access_token().await,
        Err(Error::RateLimited { .. })
    ));
    assert_eq!(ts.access_token().await.unwrap(), "at-back");
    assert_eq!(seq.calls(), 3);
    assert_eq!(stored(&*e.secrets).refresh_token.as_deref(), Some("rt"));
}

/// Shares one [`Sequence`] with the test so it can count calls.
struct SeqRef(Arc<Sequence>);
impl wiremock::Respond for SeqRef {
    fn respond(&self, r: &wiremock::Request) -> ResponseTemplate {
        self.0.respond(r)
    }
}

#[tokio::test]
async fn regression_token_endpoint_retry_after_http_date() {
    let e = env().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Date", "Wed, 21 Oct 2026 07:28:00 GMT")
                .insert_header("Retry-After", "Wed, 21 Oct 2026 07:28:30 GMT"),
        )
        .mount(&e.server)
        .await;
    let err = e.client.refresh("rt").await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(30_000)
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn regression_transient_refresh_failure_keeps_unexpired_token() {
    let e = env().await;
    Mock::given(method("POST"))
        .respond_with(server_error(503, None))
        .expect(3)
        .mount(&e.server)
        .await;
    // Inside the refresh margin but not yet expired: an outage must not fail the sync.
    seed(&*e.secrets, "at-still-good", Some("rt"), Some(now() + 90));
    let ts = source(&e);
    assert_eq!(ts.access_token().await.unwrap(), "at-still-good");
    assert_eq!(ts.access_token().await.unwrap(), "at-still-good");
    // Once really expired, the outage is surfaced (as transient, not as "reconnect").
    seed(&*e.secrets, "at-dead", Some("rt"), Some(now() - 1));
    let ts = source(&e);
    assert!(matches!(ts.access_token().await, Err(Error::Network(_))));
}

#[tokio::test]
async fn regression_expires_in_as_string_or_float() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=str&"))
        .respond_with(ok("a", None, "3599".into()))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=float&"))
        .respond_with(ok("b", None, 3599.5.into()))
        .mount(&e.server)
        .await;
    for rt in ["str", "float"] {
        let t = e.client.refresh(rt).await.unwrap();
        let at = t.expires_at.unwrap();
        assert!((now() + 3590..=now() + 3600).contains(&at), "{rt}: {at}");
    }
}

#[tokio::test]
async fn regression_nonpositive_expires_in_does_not_refresh_every_call() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-1"))
        .respond_with(ok("at-2", None, 0.into()))
        .expect(1)
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=neg"))
        .respond_with(ok("n", None, (-30).into()))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=junk"))
        .respond_with(ok("j", None, "soon".into()))
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-1", Some("rt-1"), Some(now() - 1));
    let ts = source(&e);
    for _ in 0..5 {
        assert_eq!(ts.access_token().await.unwrap(), "at-2");
    }
    assert_eq!(stored(&*e.secrets).expires_at, None);
    assert_eq!(e.client.refresh("neg").await.unwrap().expires_at, None);
    assert_eq!(e.client.refresh("junk").await.unwrap().expires_at, None);
}

#[tokio::test]
async fn absurd_expires_in_is_clamped() {
    let e = env().await;
    Mock::given(method("POST"))
        .respond_with(ok("a", None, u64::MAX.into()))
        .mount(&e.server)
        .await;
    let t = e.client.refresh("rt").await.unwrap();
    let at = t.expires_at.unwrap();
    assert!(at > now() && at <= now() + 10 * 366 * 86_400, "{at}");
}

/// A keychain whose writes fail while `fail_writes` > 0.
#[derive(Default)]
struct FlakySecrets {
    inner: MemSecrets,
    fail_writes: AtomicUsize,
}
impl SecretStore for FlakySecrets {
    fn get(&self, a: &str) -> Result<Option<String>> {
        self.inner.get(a)
    }
    fn set(&self, a: &str, v: &str) -> Result<()> {
        if self
            .fail_writes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(Error::Storage("keychain locked".into()));
        }
        self.inner.set(a, v)
    }
    fn delete(&self, a: &str) -> Result<()> {
        self.inner.delete(a)
    }
}

#[tokio::test]
async fn regression_rotated_tokens_survive_a_failed_keychain_write() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-1&"))
        .respond_with(ok("at-2", Some("rt-2"), 3600.into()))
        .expect(1)
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-2&"))
        .respond_with(ok("at-3", Some("rt-3"), 3600.into()))
        .expect(1)
        .mount(&e.server)
        .await;
    let secrets = Arc::new(FlakySecrets::default());
    seed(&secrets.inner, "at-1", Some("rt-1"), Some(now() - 1));
    secrets.fail_writes.store(1, Ordering::SeqCst);
    let ts = TokenSource::new(e.client.clone(), secrets.clone(), ACCOUNT);
    // The refresh worked; the keychain write did not. rt-1 is dead at the provider now, so the
    // new set must be used from memory, not re-read from the keychain.
    assert_eq!(ts.access_token().await.unwrap(), "at-2");
    // The API rejects at-2: refresh with rt-2 (the old rt-1 would be invalid_grant). The write
    // is retried and lands.
    assert_eq!(ts.force_refresh("at-2").await.unwrap(), "at-3");
    assert_eq!(
        stored(&secrets.inner).refresh_token.as_deref(),
        Some("rt-3")
    );
}

#[tokio::test]
async fn regression_unsaved_tokens_are_written_back_on_next_use() {
    let e = env().await;
    Mock::given(method("POST"))
        .respond_with(ok("at-2", Some("rt-2"), 3600.into()))
        .expect(1)
        .mount(&e.server)
        .await;
    let secrets = Arc::new(FlakySecrets::default());
    seed(&secrets.inner, "at-1", Some("rt-1"), Some(now() - 1));
    secrets.fail_writes.store(1, Ordering::SeqCst);
    let ts = TokenSource::new(e.client.clone(), secrets.clone(), ACCOUNT);
    assert_eq!(ts.access_token().await.unwrap(), "at-2");
    assert_eq!(
        stored(&secrets.inner).refresh_token.as_deref(),
        Some("rt-1")
    );
    ts.clear_cache().await; // must not drop the only good copy
    assert_eq!(ts.access_token().await.unwrap(), "at-2");
    assert_eq!(
        stored(&secrets.inner).refresh_token.as_deref(),
        Some("rt-2")
    );
}

#[tokio::test]
async fn tokens_stay_in_the_secret_store_and_out_of_errors_logs_and_egress() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-live&"))
        .respond_with(ok("at-SECRET", Some("rt-SECRET"), 3600.into()))
        .mount(&e.server)
        .await;
    // An error body echoing the token back must not end up in the error text.
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-echo&"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_request", "refresh_token": "rt-echo", "access_token": "at-SECRET"
        })))
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-old", Some("rt-live"), Some(now() - 1));
    let ts = source(&e);
    assert_eq!(ts.access_token().await.unwrap(), "at-SECRET");
    let dbg = format!("{ts:?} {:?} {:?}", stored(&*e.secrets), e.client);
    assert!(!dbg.contains("SECRET") && !dbg.contains("s3cret"), "{dbg}");
    let err = e.client.refresh("rt-echo").await.unwrap_err().to_string();
    assert!(
        !err.contains("rt-echo") && !err.contains("at-SECRET"),
        "{err}"
    );
    // The only place the tokens live is the store, under the account.
    let raw = e.secrets.get(ACCOUNT).unwrap().unwrap();
    assert!(raw.contains("at-SECRET") && raw.contains("rt-SECRET"));
    assert_eq!(e.secrets.0.lock().unwrap().len(), 1);
    // Every token-endpoint call is in the egress ledger under the connector's feature.
    e.egress.assert_one_row_per_request(2, "sync:gmail");
    for ev in e.egress.events() {
        assert!(ev.host.starts_with("127.0.0.1"));
        assert!(ev.items.is_empty());
        assert!(ev.bytes_sent > 0);
    }
}
