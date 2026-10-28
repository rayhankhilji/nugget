use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nugget_connector::SecretStore;
use nugget_connector::http::MeteredHttp;
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::{Error, Result};
use sha2::{Digest, Sha256};
use url::Url;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::*;

#[derive(Default)]
struct VecSink(Mutex<Vec<EgressEvent>>);
impl EgressSink for VecSink {
    fn record(&self, e: EgressEvent) {
        self.0.lock().unwrap().push(e);
    }
}

#[derive(Default)]
struct MemSecrets(Mutex<HashMap<String, String>>);
impl SecretStore for MemSecrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<()> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

const ACCOUNT: &str = "oauth:gmail:test";

struct Env {
    server: MockServer,
    egress: Arc<VecSink>,
    secrets: Arc<MemSecrets>,
    client: OAuthClient,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let egress = Arc::new(VecSink::default());
    let http = MeteredHttp::new(egress.clone(), "sync:gmail").unwrap();
    let provider = google::provider_with_endpoints(
        &format!("{}/auth", server.uri()),
        &format!("{}/token", server.uri()),
        &[google::SCOPE_GMAIL_READONLY],
    );
    let creds = ClientCredentials::new("cid.apps.googleusercontent.com", Some("s3cret".into()));
    let client = OAuthClient::new(provider, creds, http).unwrap();
    Env {
        server,
        egress,
        secrets: Arc::new(MemSecrets::default()),
        client,
    }
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
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

fn stored(secrets: &MemSecrets) -> TokenSet {
    serde_json::from_str(&secrets.get(ACCOUNT).unwrap().unwrap()).unwrap()
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
    let http = MeteredHttp::new(Arc::new(VecSink::default()), "sync:x").unwrap();
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
    let t = stored(&e.secrets);
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

#[test]
fn default_timeout_is_five_minutes() {
    assert_eq!(
        AuthorizeOptions::default().timeout,
        Duration::from_secs(300)
    );
}

fn seed(secrets: &MemSecrets, access: &str, refresh: Option<&str>, expires_at: i64) {
    let t = TokenSet {
        access_token: access.into(),
        refresh_token: refresh.map(Into::into),
        expires_at: Some(expires_at),
        token_type: Some("Bearer".into()),
        scope: None,
    };
    secrets
        .set(ACCOUNT, &serde_json::to_string(&t).unwrap())
        .unwrap();
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
    seed(&e.secrets, "at-old", Some("rt-old"), now() + 30);
    let ts = TokenSource::new(e.client.clone(), e.secrets.clone(), ACCOUNT);
    assert_eq!(ts.access_token().await.unwrap(), "at-new");
    // Cached afterwards: no second refresh.
    assert_eq!(ts.access_token().await.unwrap(), "at-new");
    let t = stored(&e.secrets);
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
    seed(&e.secrets, "at-ok", Some("rt"), now() + 3600);
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
    seed(&e.secrets, "at-old", Some("rt-revoked"), now() - 10);
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
    seed(&e.secrets, "at-old", None, now() - 10);
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
    seed(&e.secrets, "at-1", Some("rt-1"), now() + 3600);
    let ts = TokenSource::new(e.client.clone(), e.secrets.clone(), ACCOUNT);
    assert_eq!(ts.access_token().await.unwrap(), "at-1");
    assert_eq!(ts.force_refresh("at-1").await.unwrap(), "at-2");
    // A stale rejection (token already replaced) does not refresh again.
    assert_eq!(ts.force_refresh("at-1").await.unwrap(), "at-2");
    assert_eq!(stored(&e.secrets).refresh_token.as_deref(), Some("rt-2"));
}

#[tokio::test]
async fn token_endpoint_errors_map_to_typed_errors() {
    let e = env().await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=rl"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=boom"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("refresh_token=badclient"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({"error": "invalid_client"})),
        )
        .mount(&e.server)
        .await;
    assert!(matches!(
        e.client.refresh("rl").await,
        Err(Error::RateLimited {
            retry_after_ms: Some(2000)
        })
    ));
    assert!(matches!(
        e.client.refresh("boom").await,
        Err(Error::Network(_))
    ));
    assert!(matches!(
        e.client.refresh("badclient").await,
        Err(Error::Unauthorized(_))
    ));
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
