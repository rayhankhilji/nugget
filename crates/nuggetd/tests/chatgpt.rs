//! Sign in with ChatGPT through IPC (`ai.chatgpt.*`) against a fake authorization server and a
//! fake Responses API: the loopback sign-in (state checked, client id saved before the code
//! exchange, ID token and scopes verified), routing on the light and smart tiers with the
//! plan's token, the usage-limit pause without falling back to another provider, resume,
//! sign-out keeping the registration, scope-gating and cloud mode.

mod common;

use aws_lc_rs::rsa::{KeyPair, KeySize, PublicKeyComponents};
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::*;
use nugget_connector::SecretStore;
use nugget_infer::chatgpt::Endpoints;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Issuer(KeyPair);

impl Issuer {
    fn jwks(&self) -> Value {
        let c = PublicKeyComponents::<Vec<u8>>::from(self.0.public_key());
        json!({"keys": [{"kty": "RSA", "kid": "k1", "alg": "RS256",
                         "n": URL_SAFE_NO_PAD.encode(&c.n), "e": URL_SAFE_NO_PAD.encode(&c.e)}]})
    }
    fn id_token(&self, aud: &str, nonce: &str) -> String {
        let h = URL_SAFE_NO_PAD.encode(json!({"alg": "RS256", "kid": "k1"}).to_string());
        let exp = now() + 3600;
        let p = URL_SAFE_NO_PAD.encode(
            json!({"iss": "https://auth.openai.com", "aud": aud, "sub": "u1", "nonce": nonce,
                   "exp": exp, "email": "ann@example.com", "name": "Ann Lee"})
            .to_string(),
        );
        let msg = format!("{h}.{p}");
        let mut sig = vec![0u8; self.0.public_modulus_len()];
        self.0
            .sign(
                &RSA_PKCS1_SHA256,
                &aws_lc_rs::rand::SystemRandom::new(),
                msg.as_bytes(),
                &mut sig,
            )
            .unwrap();
        format!("{msg}.{}", URL_SAFE_NO_PAD.encode(sig))
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

const FULL: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

fn responses_sse(text: &str) -> ResponseTemplate {
    let body: String = [
        json!({"type": "response.created", "response": {"id": "r"}}),
        json!({"type": "response.output_text.delta", "delta": text}),
        json!({"type": "response.completed", "response": {"id": "r"}}),
    ]
    .iter()
    .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
    .collect();
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

struct Fake {
    server: MockServer,
    issuer: Issuer,
}

impl Fake {
    async fn start() -> Self {
        let server = MockServer::start().await;
        let issuer = Issuer(KeyPair::generate(KeySize::Rsa2048).unwrap());
        Mock::given(method("GET"))
            .and(path("/.well-known/jwks.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(issuer.jwks()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer at-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
                {"slug": "gpt-sol", "visibility": "list"}, {"slug": "gpt-sol-mini", "visibility": "list"}]})))
            .mount(&server)
            .await;
        Self { server, issuer }
    }

    fn endpoints(&self) -> Endpoints {
        let u = self.server.uri();
        Endpoints {
            issuer: "https://auth.openai.com".into(),
            authorize: "https://auth.openai.com/api/accounts/authorize".into(),
            token: format!("{u}/oauth/token"),
            revoke: format!("{u}/oauth/revoke"),
            jwks: format!("{u}/.well-known/jwks.json"),
            api: format!("{u}/v1"),
        }
    }

    /// The token endpoint answers the code exchange for `client` and `nonce` with `scope`.
    async fn token(&self, client: &str, nonce: &str, scope: &str) {
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains(format!("client_id={client}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600,
                "scope": scope, "id_token": self.issuer.id_token(client, nonce)})))
            .mount(&self.server)
            .await;
    }
}

fn query(url: &str) -> Vec<(String, String)> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

fn param(q: &[(String, String)], k: &str) -> Option<String> {
    q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
}

async fn start(fake: &Fake) -> Harness {
    let config = format!("{NO_RUNTIME}\n");
    let ep = fake.endpoints();
    Harness::start_with(&config, move |o| o.chatgpt = Some(ep)).await
}

async fn status(h: &Harness) -> Value {
    h.call("ai.chatgpt.status", json!({})).await
}

/// Starts a sign-in and returns (auth_url params, redirect_uri).
async fn begin(h: &Harness) -> (Vec<(String, String)>, String) {
    let v: Value = h.call("ai.chatgpt.sign_in", json!({})).await;
    assert_eq!(v["status"]["state"], "signing_in", "{v}");
    let q = query(v["auth_url"].as_str().unwrap());
    let redirect = param(&q, "redirect_uri").unwrap();
    assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/auth/callback"));
    (q, redirect)
}

async fn browser(url: &str) -> reqwest::StatusCode {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(url)
        .send()
        .await
        .unwrap()
        .status()
}

async fn until_not_signing_in(h: &Harness) -> Value {
    for _ in 0..100 {
        let s = status(h).await;
        if s["state"] != "signing_in" {
            return s;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("sign-in did not finish");
}

#[tokio::test]
async fn sign_in_routes_the_plan_pauses_at_the_limit_and_signs_out() {
    let fake = Fake::start().await;
    let h = start(&fake).await;
    assert_eq!(status(&h).await["state"], "signed_out");

    let (q, redirect) = begin(&h).await;
    assert_eq!(
        param(&q, "client_id").as_deref(),
        Some("dynamic_agent_client")
    );
    assert_eq!(param(&q, "agent_name_hint").as_deref(), Some("Heed"));
    let host_id = param(&q, "ext_agent_host_id").unwrap();
    assert!(host_id.starts_with("urn:uuid:"));
    assert_eq!(param(&q, "scope").as_deref(), Some(FULL));
    let (state, nonce) = (param(&q, "state").unwrap(), param(&q, "nonce").unwrap());
    fake.token("cid_1", &nonce, FULL).await;

    // A callback with another state is answered and ignored; the sign-in keeps waiting.
    assert_eq!(
        browser(&format!("{redirect}?code=x&state=forged&client_id=evil")).await,
        400
    );
    assert_eq!(status(&h).await["state"], "signing_in");
    assert_eq!(
        browser(&format!("{redirect}?code=c1&state={state}&client_id=cid_1")).await,
        200
    );
    let s = until_not_signing_in(&h).await;
    assert_eq!(s["state"], "signed_in", "{s}");
    assert_eq!(
        (s["plan_usage"].as_bool(), s["paused"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(s["account"]["email"], "ann@example.com");
    assert_eq!(
        s["models"],
        json!({"light": "gpt-sol-mini", "smart": "gpt-sol"})
    );
    assert!(s["error"].is_null(), "{s}");
    assert!(
        !s.to_string().contains("at-1") && !s.to_string().contains("rt-1"),
        "no tokens in status"
    );

    // Host id and issued client id on disk (owner-only); tokens only in the keychain.
    let host_file = h.paths.data.join("chatgpt-host.json");
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(&host_file).unwrap()).unwrap();
    assert_eq!(
        (rec["host_id"].as_str(), rec["client_id"].as_str()),
        (Some(host_id.as_str()), Some("cid_1"))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&host_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let creds = h
        .secrets
        .get("chatgpt:credentials")
        .unwrap()
        .expect("credentials in the keychain");
    assert!(creds.contains("rt-1"));
    assert!(
        !std::fs::read_to_string(h.paths.config_file())
            .unwrap()
            .contains("at-1")
    );

    // The plan serves the smart tier with its token, store off.
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer at-1"))
        .respond_with(responses_sse("from chatgpt"))
        .up_to_n_times(1)
        .mount(&fake.server)
        .await;
    let watcher = collect(&h.client);
    let (started, text) = h.chat(&watcher, chat_params("synthesis", "hello")).await;
    assert_eq!(started.unwrap().provider.to_string(), "chatgpt-smart");
    assert_eq!(text.unwrap(), "from chatgpt");
    let reqs = fake.server.received_requests().await.unwrap();
    let body: Value = reqs
        .iter()
        .rev()
        .find(|r| r.url.path() == "/v1/responses")
        .unwrap()
        .body_json()
        .unwrap();
    assert_eq!(
        (body["store"].clone(), body["model"].clone()),
        (json!(false), json!("gpt-sol"))
    );
    let listed: Value = h.call("providers.list", json!({})).await;
    assert!(listed.to_string().contains("chatgpt-light"), "{listed}");

    // The usage limit: a clear error, paused, nothing else asked.
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({"error": {
            "code": "subscription_sharing_usage_limit_exceeded", "message": "limit"}})))
        .mount(&fake.server)
        .await;
    let watcher = collect(&h.client);
    let (_, text) = h.chat(&watcher, chat_params("synthesis", "again")).await;
    let e = text.unwrap_err();
    assert!(
        e.contains("subscription_sharing_usage_limit_exceeded")
            && e.contains("chatgpt.com/settings/usage"),
        "{e}"
    );
    let s = status(&h).await;
    assert_eq!(
        (s["paused"].as_bool(), s["error"]["code"].as_str()),
        (Some(true), Some("usage_limit")),
        "{s}"
    );
    let s: Value = h.call("ai.chatgpt.resume", json!({})).await;
    assert_eq!(
        (s["paused"].as_bool(), s["error"].is_null()),
        (Some(false), true),
        "{s}"
    );

    // Sign-out: tokens revoked and gone, providers off, the registration kept.
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(body_string_contains("token=rt-1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&fake.server)
        .await;
    let s: Value = h.call("ai.chatgpt.sign_out", json!({})).await;
    assert_eq!(s["state"], "signed_out");
    assert!(h.secrets.get("chatgpt:credentials").unwrap().is_none());
    let listed: Value = h.call("providers.list", json!({})).await;
    assert!(!listed.to_string().contains("chatgpt-"), "{listed}");
    let (q, _) = begin(&h).await;
    assert_eq!(
        param(&q, "client_id").as_deref(),
        Some("cid_1"),
        "the registration is reused"
    );
    assert_eq!(param(&q, "agent_name_hint"), None);
    assert_eq!(
        param(&q, "ext_agent_host_id").as_deref(),
        Some(host_id.as_str()),
        "same host id"
    );
    let s: Value = h.call("ai.chatgpt.cancel", json!({})).await;
    let _ = s;
    let s = until_not_signing_in(&h).await;
    assert_eq!(
        (s["state"].as_str(), s["error"]["code"].as_str()),
        (Some("signed_out"), Some("cancelled"))
    );
    let audit = h.audit();
    assert!(audit.iter().any(|(a, _, _)| a == "chatgpt.sign_in"));
    assert!(
        audit
            .iter()
            .all(|(_, _, d)| !d.to_string().contains("rt-1")),
        "no tokens in the audit"
    );
}

#[tokio::test]
async fn without_plan_usage_nothing_is_routed() {
    let fake = Fake::start().await;
    let h = start(&fake).await;
    let (q, redirect) = begin(&h).await;
    let (state, nonce) = (param(&q, "state").unwrap(), param(&q, "nonce").unwrap());
    fake.token("cid_2", &nonce, "openid profile email offline_access")
        .await;
    assert_eq!(
        browser(&format!("{redirect}?code=c&state={state}&client_id=cid_2")).await,
        200
    );
    let s = until_not_signing_in(&h).await;
    assert_eq!(s["state"], "signed_in", "{s}");
    assert_eq!(s["plan_usage"], false);
    assert_eq!(s["error"]["code"], "scope_not_granted");
    let listed: Value = h.call("providers.list", json!({})).await;
    assert!(
        !listed.to_string().contains("chatgpt-"),
        "nothing routed: {listed}"
    );
}

#[tokio::test]
async fn declined_and_unverifiable_sign_ins_store_nothing() {
    let fake = Fake::start().await;
    let h = start(&fake).await;
    let (q, redirect) = begin(&h).await;
    let state = param(&q, "state").unwrap();
    assert_eq!(
        browser(&format!("{redirect}?error=access_denied&state={state}")).await,
        200
    );
    let s = until_not_signing_in(&h).await;
    assert_eq!(
        (s["state"].as_str(), s["error"]["code"].as_str()),
        (Some("signed_out"), Some("access_denied"))
    );

    // An ID token for another nonce (a replayed or forged answer) is refused.
    let (q, redirect) = begin(&h).await;
    let state = param(&q, "state").unwrap();
    fake.token("cid_3", "some-other-nonce", FULL).await;
    assert_eq!(
        browser(&format!("{redirect}?code=c&state={state}&client_id=cid_3")).await,
        200
    );
    let s = until_not_signing_in(&h).await;
    assert_eq!(
        (s["state"].as_str(), s["error"]["code"].as_str()),
        (Some("signed_out"), Some("invalid_id_token")),
        "{s}"
    );
    assert!(h.secrets.get("chatgpt:credentials").unwrap().is_none());
}

#[tokio::test]
async fn not_while_heed_cloud_serves_this_computer() {
    let fake = Fake::start().await;
    let h = start(&fake).await;
    let cloud = MockServer::start().await;
    let _: Value = h
        .call(
            "cloud.llm.session",
            json!({"api_base": cloud.uri(), "access_token": "cloud-token", "expires_at": now() + 600}),
        )
        .await;
    let v: Value = h.call("ai.chatgpt.sign_in", json!({})).await;
    assert!(v["auth_url"].is_null(), "{v}");
    assert_eq!(v["status"]["error"]["code"], "cloud_mode");
}
