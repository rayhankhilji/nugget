use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nugget_connector::SecretStore;
use nugget_connector::http::MeteredHttp;
use nugget_connector::testkit::{MemSecrets, Sequence, VecEgress, server_error, too_many_requests};
use nugget_core::{Error, Result};
use sha2::{Digest, Sha256};
use url::Url;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::*;

const ACCOUNT: &str = "oauth:gmail:test";

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

fn query(url: &str) -> HashMap<String, String> {
    Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

fn token_json(access: &str, refresh: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "access_token": access, "expires_in": 3599, "token_type": "Bearer",
        "scope": google::SCOPE_GMAIL_READONLY
    });
    if let Some(r) = refresh {
        v["refresh_token"] = r.into();
    }
    v
}

#[test]
fn pkce_matches_rfc7636_vector() {
    let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
    assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    let g = Pkce::generate().unwrap();
    assert_eq!(g.verifier.len(), 64);
    assert_ne!(g.verifier, Pkce::generate().unwrap().verifier);
}

#[test]
fn debug_output_redacts_secrets() {
    let creds = ClientCredentials::new("id", Some("topsecret".into()));
    assert!(!format!("{creds:?}").contains("topsecret"));
    let t = TokenSet {
        access_token: "ya29.secret".into(),
        refresh_token: Some("1//refresh".into()),
        expires_at: None,
        token_type: None,
        scope: None,
    };
    let d = format!("{t:?}");
    assert!(!d.contains("ya29") && !d.contains("1//refresh"));
}

#[test]
fn rejects_bad_client_config() {
    let http = MeteredHttp::new(Arc::new(VecEgress::default()), "sync:x").unwrap();
    let p = google::provider(&[google::SCOPE_GMAIL_READONLY]);
    assert!(OAuthClient::new(p.clone(), ClientCredentials::new(" ", None), http.clone()).is_err());
    let mut bad = p;
    bad.token_url = "ftp://x".into();
    assert!(OAuthClient::new(bad, ClientCredentials::new("id", None), http).is_err());
}

#[tokio::test]
async fn full_pkce_loopback_flow_stores_tokens() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains("client_secret=s3cret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("at-1", Some("rt-1"))))
        .expect(1)
        .mount(&e.server)
        .await;

    let opts = AuthorizeOptions {
        login_hint: Some("me@example.com".into()),
        ..Default::default()
    };
    let flow = e
        .client
        .begin(e.secrets.clone(), ACCOUNT, opts)
        .await
        .unwrap();
    let q = query(&flow.url);
    assert!(flow.url.starts_with(&format!("{}/auth?", e.server.uri())));
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "cid.apps.googleusercontent.com");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["scope"], google::SCOPE_GMAIL_READONLY);
    assert_eq!(q["access_type"], "offline");
    assert_eq!(q["login_hint"], "me@example.com");
    assert!(!q.contains_key("client_secret"));
    let redirect = q["redirect_uri"].clone();
    assert!(redirect.starts_with("http://127.0.0.1:"));
    let state = q["state"].clone();
    assert!(state.len() >= 40);

    let done = tokio::spawn(flow.done);

    // A browser pre-connect that never sends anything must not block the real redirect.
    let addr = Url::parse(&redirect).unwrap();
    let _idle = tokio::net::TcpStream::connect((addr.host_str().unwrap(), addr.port().unwrap()))
        .await
        .unwrap();

    let http = reqwest::Client::new();
    let favicon = http
        .get(format!("{redirect}favicon.ico"))
        .send()
        .await
        .unwrap();
    assert_eq!(favicon.status(), 404);

    let resp = http
        .get(format!("{redirect}?state={state}&code=the-code&scope=x"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("You can close this tab")
    );

    done.await.unwrap().unwrap();
    let t = stored(&*e.secrets);
    assert_eq!(t.access_token, "at-1");
    assert_eq!(t.refresh_token.as_deref(), Some("rt-1"));
    assert!(t.expires_at.unwrap() > now() + 3000);

    // The verifier sent to the token endpoint matches the challenge in the URL.
    let reqs = e.server.received_requests().await.unwrap();
    let body: HashMap<String, String> = url::form_urlencoded::parse(&reqs[0].body)
        .into_owned()
        .collect();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(body["code_verifier"].as_bytes()));
    assert_eq!(challenge, q["code_challenge"]);
    assert_eq!(body["redirect_uri"], redirect);

    // The token exchange was metered.
    let ev = e.egress.0.lock().unwrap();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].feature, "sync:gmail");
}

#[tokio::test]
async fn forged_state_is_ignored_and_the_flow_keeps_waiting() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("code=real-code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("at", Some("rt"))))
        .expect(1)
        .mount(&e.server)
        .await;
    let auth = e
        .client
        .authorize(&AuthorizeOptions::default())
        .await
        .unwrap();
    let state = query(&auth.url)["state"].clone();
    let redirect = auth.redirect_uri.clone();
    let done = tokio::spawn(auth.finish());
    // Any local process can hit the port: forged, missing and error-carrying callbacks with a
    // wrong state get a 400 and do not end the flow.
    for q in [
        "state=forged&code=c",
        "code=c",
        "state=forged&error=access_denied",
    ] {
        let resp = reqwest::get(format!("{redirect}?{q}")).await.unwrap();
        assert_eq!(resp.status(), 400, "{q}");
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !done.is_finished(),
        "a forged redirect must not abort the flow"
    );
    let resp = reqwest::get(format!("{redirect}?state={state}&code=real-code"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let tokens = done.await.unwrap().unwrap();
    assert_eq!(tokens.access_token, "at");
}

#[tokio::test]
async fn forged_state_is_reported_when_the_flow_times_out() {
    let e = env().await;
    let opts = AuthorizeOptions {
        timeout: Duration::from_millis(500),
        ..Default::default()
    };
    let auth = e.client.authorize(&opts).await.unwrap();
    let redirect = auth.redirect_uri.clone();
    let done = tokio::spawn(auth.finish());
    let resp = reqwest::get(format!("{redirect}?state=forged&code=c"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let err = done.await.unwrap().unwrap_err();
    assert!(
        matches!(err, Error::Unauthorized(ref m) if m.contains("timed out") && m.contains("wrong OAuth state")),
        "{err}"
    );
}

#[tokio::test]
async fn idle_connections_beyond_the_cap_do_not_grow_unbounded() {
    let e = env().await;
    let auth = e
        .client
        .authorize(&AuthorizeOptions::default())
        .await
        .unwrap();
    let state = query(&auth.url)["state"].clone();
    let redirect = auth.redirect_uri.clone();
    let u = Url::parse(&redirect).unwrap();
    let addr = (u.host_str().unwrap().to_string(), u.port().unwrap());
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("at", None)))
        .mount(&e.server)
        .await;
    let done = tokio::spawn(auth.finish());
    // 16 idle connections fill the cap; a 17th is left in the backlog (not served).
    let mut idle = Vec::new();
    for _ in 0..16 {
        idle.push(tokio::net::TcpStream::connect(addr.clone()).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let blocked = tokio::spawn({
        let url = format!("{redirect}?state={state}&code=c");
        async move { reqwest::get(url).await.map(|r| r.status()) }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!blocked.is_finished(), "served beyond the connection cap");
    // Freeing the slots lets the real redirect through.
    drop(idle);
    let status = tokio::time::timeout(Duration::from_secs(5), blocked)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(status, 200);
    done.await.unwrap().unwrap();
}

#[tokio::test]
async fn denied_consent_is_unauthorized() {
    let e = env().await;
    let auth = e
        .client
        .authorize(&AuthorizeOptions::default())
        .await
        .unwrap();
    let state = query(&auth.url)["state"].clone();
    let redirect = auth.redirect_uri.clone();
    let done = tokio::spawn(auth.finish());
    reqwest::get(format!("{redirect}?state={state}&error=access_denied"))
        .await
        .unwrap();
    assert!(matches!(
        done.await.unwrap(),
        Err(Error::Unauthorized(m)) if m.contains("access_denied")
    ));
}

#[tokio::test]
async fn redirect_wait_times_out() {
    let e = env().await;
    let opts = AuthorizeOptions {
        timeout: Duration::from_millis(150),
        ..Default::default()
    };
    let auth = e.client.authorize(&opts).await.unwrap();
    let err = auth.finish().await.unwrap_err();
    assert!(
        matches!(err, Error::Unauthorized(ref m) if m.contains("timed out")),
        "{err}"
    );
}

#[tokio::test]
async fn token_source_refreshes_before_expiry_and_keeps_refresh_token() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=rt-old"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("at-new", None)))
        .expect(1)
        .mount(&e.server)
        .await;
    // Expires in 30 s: inside the refresh skew.
    seed(&*e.secrets, "at-old", Some("rt-old"), Some(now() + 30));
    let ts = TokenSource::new(e.client.clone(), e.secrets.clone(), ACCOUNT);
    assert_eq!(ts.access_token().await.unwrap(), "at-new");
    // Cached afterwards: no second refresh.
    assert_eq!(ts.access_token().await.unwrap(), "at-new");
    let t = stored(&*e.secrets);
    assert_eq!(t.access_token, "at-new");
    assert_eq!(t.refresh_token.as_deref(), Some("rt-old"));
    assert_eq!(e.egress.0.lock().unwrap()[0].feature, "sync:gmail");
}

#[tokio::test]
async fn valid_token_is_used_without_refresh() {
    let e = env().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-ok", Some("rt"), Some(now() + 3600));
    let ts = TokenSource::new(e.client.clone(), e.secrets.clone(), ACCOUNT);
    assert_eq!(ts.access_token().await.unwrap(), "at-ok");
    assert!(ts.is_connected().await.unwrap());
}

#[tokio::test]
async fn invalid_grant_is_unauthorized() {
    let e = env().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "Token has been expired or revoked."
        })))
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-old", Some("rt-revoked"), Some(now() - 10));
    let ts = TokenSource::new(e.client.clone(), e.secrets.clone(), ACCOUNT);
    let err = ts.access_token().await.unwrap_err();
    assert!(
        matches!(err, Error::Unauthorized(ref m) if m.contains("invalid_grant")),
        "{err}"
    );
}

#[tokio::test]
async fn missing_or_unrefreshable_tokens_are_unauthorized() {
    let e = env().await;
    let ts = TokenSource::new(e.client.clone(), e.secrets.clone(), ACCOUNT);
    assert!(matches!(
        ts.access_token().await,
        Err(Error::Unauthorized(_))
    ));
    assert!(!ts.is_connected().await.unwrap());
    seed(&*e.secrets, "at-old", None, Some(now() - 10));
    assert!(matches!(
        ts.access_token().await,
        Err(Error::Unauthorized(_))
    ));
    e.secrets.set(ACCOUNT, "{not json").unwrap();
    assert!(matches!(
        ts.access_token().await,
        Err(Error::Unauthorized(_))
    ));
}

#[tokio::test]
async fn force_refresh_after_401() {
    let e = env().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("at-2", Some("rt-2"))))
        .expect(1)
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-1", Some("rt-1"), Some(now() + 3600));
    let ts = TokenSource::new(e.client.clone(), e.secrets.clone(), ACCOUNT);
    assert_eq!(ts.access_token().await.unwrap(), "at-1");
    assert_eq!(ts.force_refresh("at-1").await.unwrap(), "at-2");
    // A stale rejection (token already replaced) does not refresh again.
    assert_eq!(ts.force_refresh("at-1").await.unwrap(), "at-2");
    assert_eq!(stored(&*e.secrets).refresh_token.as_deref(), Some("rt-2"));
}

#[tokio::test]
async fn token_endpoint_redirects_are_not_followed() {
    let e = env().await;
    let elsewhere = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("stolen", None)))
        .expect(0)
        .mount(&elsewhere)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/token", elsewhere.uri())),
        )
        .mount(&e.server)
        .await;
    let err = e.client.refresh("rt").await.unwrap_err();
    assert!(
        matches!(err, Error::Network(ref m) if m.contains("redirect")),
        "{err}"
    );
    assert_eq!(e.egress.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn malformed_token_responses_are_errors_not_panics() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=garbage"))
        .respond_with(ResponseTemplate::new(200).set_body_string("\u{0}not json"))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=empty"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"expires_in": 5})),
        )
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=huge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "a", "expires_in": i64::MAX
        })))
        .mount(&e.server)
        .await;
    assert!(matches!(
        e.client.refresh("garbage").await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        e.client.refresh("empty").await,
        Err(Error::Invalid(_))
    ));
    assert!(e.client.refresh("huge").await.unwrap().expires_at.is_some());
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
        (
            "rt-badclient",
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({"error": "invalid_client"})),
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
    // A rejected client is not transient.
    let err = e.client.refresh("rt-badclient").await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err}");
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
        .expect(2)
        .mount(&e.server)
        .await;
    // Inside the refresh margin but not yet expired: an outage must not fail the sync, and the
    // refresh is not retried on every call (backoff before the next try).
    seed(&*e.secrets, "at-still-good", Some("rt"), Some(now() + 90));
    let ts = source(&e);
    for _ in 0..5 {
        assert_eq!(ts.access_token().await.unwrap(), "at-still-good");
    }
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
        // `fetch_update` is `try_update` from Rust 1.99, which is newer than the MSRV.
        #[allow(deprecated)]
        let fail = self
            .fail_writes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if fail {
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
    assert_eq!(ts.access_token().await.unwrap(), "at-2");
    assert_eq!(
        stored(&secrets.inner).refresh_token.as_deref(),
        Some("rt-2")
    );
}

#[tokio::test]
async fn tokens_stay_in_the_secret_store_and_out_of_errors_debug_and_egress() {
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

#[tokio::test]
async fn regression_early_refresh_failure_honours_retry_after() {
    let e = env().await;
    Mock::given(method("POST"))
        .respond_with(too_many_requests(3600))
        .expect(1)
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-good", Some("rt"), Some(now() + 100));
    let ts = source(&e);
    for _ in 0..10 {
        assert_eq!(ts.access_token().await.unwrap(), "at-good");
    }
}

/// A refresh whose keychain write failed, then the user reconnects: the new grant wins.
async fn unsaved_then_reconnect(clear: bool) {
    let e = env().await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-1&"))
        .respond_with(ok("at-2", Some("rt-2"), 3600.into()))
        .expect(1)
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-2&"))
        .respond_with(ok("stale", None, 3600.into()))
        .expect(0)
        .mount(&e.server)
        .await;
    let secrets = Arc::new(FlakySecrets::default());
    seed(&secrets.inner, "at-1", Some("rt-1"), Some(now() - 1));
    secrets.fail_writes.store(1, Ordering::SeqCst);
    let ts = TokenSource::new(e.client.clone(), secrets.clone(), ACCOUNT);
    assert_eq!(ts.access_token().await.unwrap(), "at-2");
    // Reconnect: a fresh grant lands in the keychain.
    seed(
        &secrets.inner,
        "at-grant",
        Some("rt-grant"),
        Some(now() + 3600),
    );
    if clear {
        ts.clear_cache().await;
    }
    assert_eq!(ts.access_token().await.unwrap(), "at-grant");
    assert_eq!(ts.force_refresh("at-2").await.unwrap(), "at-grant");
    let t = stored(&secrets.inner);
    assert_eq!(
        (t.access_token.as_str(), t.refresh_token.as_deref()),
        ("at-grant", Some("rt-grant"))
    );
}

#[tokio::test]
async fn regression_reconnect_after_failed_write_is_not_undone() {
    unsaved_then_reconnect(false).await;
}

#[tokio::test]
async fn regression_clear_cache_after_reconnect_drops_unsaved_tokens() {
    unsaved_then_reconnect(true).await;
}

#[tokio::test]
async fn regression_revoked_unsaved_tokens_are_dropped() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-1&"))
        .respond_with(ok("at-2", Some("rt-2"), 1.into()))
        .expect(1)
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rt-2&"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "invalid_grant"})),
        )
        .expect(1)
        .mount(&e.server)
        .await;
    let secrets = Arc::new(FlakySecrets::default());
    seed(&secrets.inner, "at-1", Some("rt-1"), Some(now() - 1));
    secrets.fail_writes.store(10, Ordering::SeqCst);
    let ts = TokenSource::new(e.client.clone(), secrets.clone(), ACCOUNT);
    assert_eq!(ts.access_token().await.unwrap(), "at-2");
    // at-2 expires within the margin; refreshing with rt-2 is refused.
    let err = ts.force_refresh("at-2").await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    // The revoked set is gone from memory; after a reconnect the keychain is used.
    secrets.fail_writes.store(0, Ordering::SeqCst);
    seed(
        &secrets.inner,
        "at-grant",
        Some("rt-grant"),
        Some(now() + 3600),
    );
    assert_eq!(ts.access_token().await.unwrap(), "at-grant");
    assert_eq!(stored(&secrets.inner).access_token, "at-grant");
}

#[test]
fn users_own_client_wins_over_the_builtin_one_and_secrets_never_mix() {
    let builtin = Some(ClientCredentials::new("nugget-id", Some("nugget-s".into())));
    let r = bundled::resolve(Some(" mine "), None, builtin.clone()).unwrap();
    assert_eq!((r.client_id.as_str(), r.client_secret), ("mine", None));
    let r = bundled::resolve(Some(""), Some("stray".into()), builtin.clone()).unwrap();
    assert_eq!(r, builtin.clone().unwrap());
    assert_eq!(bundled::resolve(None, None, None), None);
    // Unknown providers never have a built-in client.
    assert!(bundled::client("nosuch").is_none());
}

#[test]
fn redirect_host_is_loopback_only_and_empty_scopes_are_omitted() {
    let http = MeteredHttp::new(Arc::new(VecEgress::default()), "sync:x").unwrap();
    let creds = ClientCredentials::new("id", None);
    let mut p = google::provider(&[]);
    p.redirect_host = Some("evil.example".into());
    assert!(OAuthClient::new(p.clone(), creds.clone(), http.clone()).is_err());
    p.redirect_host = Some("localhost".into());
    let c = OAuthClient::new(p, creds, http).unwrap();
    let url = c
        .authorization_url(
            "http://localhost:1/",
            "st",
            &Pkce::generate().unwrap(),
            None,
        )
        .unwrap();
    assert!(!query(&url).contains_key("scope"));
}

#[tokio::test]
async fn slack_user_tokens_and_ok_false_errors_are_understood() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("code=good"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "app_id": "A1",
            "authed_user": {"id": "U1", "scope": "search:read", "access_token": "xoxe.xoxp-1",
                            "token_type": "user", "refresh_token": "xoxe-1", "expires_in": 43200}
        })))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("code=bad"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": false, "error": "invalid_code"})),
        )
        .mount(&e.server)
        .await;
    let t = e
        .client
        .exchange_code("good", "v", "http://x/")
        .await
        .unwrap();
    assert_eq!(t.access_token, "xoxe.xoxp-1");
    assert_eq!(t.refresh_token.as_deref(), Some("xoxe-1"));
    assert!(t.expires_at.is_some());
    let err = e
        .client
        .exchange_code("bad", "v", "http://x/")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
}

#[tokio::test]
async fn disconnect_revokes_at_the_provider_and_deletes_the_tokens() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=at-live"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&e.server)
        .await;
    seed(&*e.secrets, "at-live", Some("rt"), Some(now() + 3600));
    let ts = source(&e);
    assert!(ts.disconnect().await.unwrap());
    assert!(e.secrets.get(ACCOUNT).unwrap().is_none());
    assert!(matches!(
        ts.access_token().await,
        Err(Error::Unauthorized(_))
    ));
}

#[tokio::test]
async fn disconnect_deletes_the_tokens_even_when_revoking_fails() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=rt-old"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&e.server)
        .await;
    // Expired access token: the refresh token is what gets revoked.
    seed(&*e.secrets, "at-old", Some("rt-old"), Some(now() - 10));
    assert!(!source(&e).disconnect().await.unwrap());
    assert!(e.secrets.get(ACCOUNT).unwrap().is_none());
    assert!(
        e.egress
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|r| !format!("{r:?}").contains("rt-old")),
        "tokens never reach the egress ledger"
    );
}
