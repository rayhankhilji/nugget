//! Sign in with ChatGPT against a fake authorization server and a fake Responses API.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use aws_lc_rs::rsa::{KeyPair, KeySize, PublicKeyComponents};
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256};
use futures::StreamExt;
use nugget_core::egress::EgressEvent;
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::locality::tests::FakeResolver;
use crate::router::{Router, TierMap};
use crate::types::{ChatEvent, Message};

#[derive(Default)]
struct Ledger(Mutex<Vec<EgressEvent>>);
impl EgressSink for Ledger {
    fn record(&self, e: EgressEvent) {
        self.0.lock().unwrap().push(e);
    }
}

/// A fake issuer: an RSA key, its JWKS, and signed ID tokens.
struct Issuer {
    key: KeyPair,
    kid: &'static str,
}

impl Issuer {
    fn new() -> Self {
        Self {
            key: KeyPair::generate(KeySize::Rsa2048).unwrap(),
            kid: "k1",
        }
    }

    fn jwks(&self) -> Value {
        let c = PublicKeyComponents::<Vec<u8>>::from(self.key.public_key());
        json!({"keys": [{"kty": "RSA", "kid": self.kid, "alg": "RS256", "use": "sig",
                         "n": URL_SAFE_NO_PAD.encode(&c.n), "e": URL_SAFE_NO_PAD.encode(&c.e)}]})
    }

    fn token(&self, claims: Value) -> String {
        let h = URL_SAFE_NO_PAD
            .encode(json!({"alg": "RS256", "kid": self.kid, "typ": "JWT"}).to_string());
        let p = URL_SAFE_NO_PAD.encode(claims.to_string());
        let msg = format!("{h}.{p}");
        let mut sig = vec![0u8; self.key.public_modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                msg.as_bytes(),
                &mut sig,
            )
            .unwrap();
        format!("{msg}.{}", URL_SAFE_NO_PAD.encode(sig))
    }
}

fn claims(iss: &str, aud: &str, nonce: &str, exp: i64) -> Value {
    json!({"iss": iss, "aud": aud, "sub": "user-1", "nonce": nonce, "exp": exp, "iat": exp - 3600,
           "email": "ann@example.com", "name": "Ann Lee"})
}

fn endpoints(server: &MockServer) -> Endpoints {
    let u = server.uri();
    Endpoints {
        issuer: "https://auth.openai.com".into(),
        authorize: format!("{u}/api/accounts/authorize"),
        token: format!("{u}/api/accounts/oauth/token"),
        revoke: format!("{u}/api/accounts/oauth/revoke"),
        jwks: format!("{u}/.well-known/jwks.json"),
        api: format!("{u}/v1"),
    }
}

#[test]
fn host_ids_are_opaque_v4_urns() {
    let a = new_host_id().unwrap();
    let b = new_host_id().unwrap();
    assert_ne!(a, b);
    assert!(is_host_id(&a), "{a}");
    assert_eq!(&a[23..24], "4", "version 4: {a}");
    for bad in [
        "ann@example.com",
        "urn:uuid:ZZ",
        "uuid",
        "urn:uuid:01234567-89AB-4def-8123-456789abcdef",
    ] {
        assert!(!is_host_id(bad), "{bad}");
    }
}

#[test]
fn pkce_matches_rfc_7636() {
    assert_eq!(
        pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let a = Attempt::new("http://127.0.0.1:1/auth/callback".into()).unwrap();
    let b = Attempt::new("http://127.0.0.1:1/auth/callback".into()).unwrap();
    assert!((43..=128).contains(&a.verifier.len()));
    assert_ne!(a.state, b.state, "fresh state every time");
    assert_ne!(a.nonce, b.nonce);
    assert_ne!(a.verifier, b.verifier);
    assert!(
        !format!("{a:?}").contains(&a.verifier),
        "Debug hides secrets"
    );
}

#[test]
fn authorize_url_registers_then_reuses_the_client() {
    let ep = Endpoints::default();
    let a = Attempt::new("http://127.0.0.1:5555/auth/callback".into()).unwrap();
    let host = "urn:uuid:01234567-89ab-4def-8123-456789abcdef";
    let q = |u: &str| -> Vec<(String, String)> {
        url::Url::parse(u)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    };
    let get =
        |p: &[(String, String)], k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let first = authorize_url(&ep, None, host, &a, None).unwrap();
    assert!(first.starts_with("https://auth.openai.com/api/accounts/authorize?"));
    let p = q(&first);
    assert_eq!(get(&p, "client_id").as_deref(), Some(DYNAMIC_CLIENT_ID));
    assert_eq!(get(&p, "agent_name_hint").as_deref(), Some("Heed"));
    assert_eq!(get(&p, "ext_agent_host_id").as_deref(), Some(host));
    assert_eq!(get(&p, "scope").as_deref(), Some(SCOPES));
    assert_eq!(
        get(&p, "resource").as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(get(&p, "code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(get(&p, "code_challenge"), Some(a.challenge()));
    assert_eq!(
        get(&p, "redirect_uri").as_deref(),
        Some("http://127.0.0.1:5555/auth/callback")
    );
    assert_eq!(
        (get(&p, "state"), get(&p, "nonce")),
        (Some(a.state.clone()), Some(a.nonce.clone()))
    );
    let again = q(&authorize_url(&ep, Some("cid_123"), host, &a, Some("ann@example.com")).unwrap());
    assert_eq!(get(&again, "client_id").as_deref(), Some("cid_123"));
    assert_eq!(
        get(&again, "agent_name_hint"),
        None,
        "registration only once"
    );
    assert_eq!(
        get(&again, "login_hint").as_deref(),
        Some("ann@example.com")
    );
    for s in [
        "offline_access",
        "resource.invoke",
        "chatgpt.tokens.use.direct",
    ] {
        assert!(SCOPES.split(' ').any(|x| x == s), "{s}");
    }
}

#[test]
fn callbacks_check_state_first() {
    assert_eq!(
        parse_callback("code=c1&state=s1&client_id=cid_9", "s1"),
        Ok(Callback::Code {
            code: "c1".into(),
            client_id: Some("cid_9".into())
        })
    );
    assert_eq!(
        parse_callback("code=c1&state=evil", "s1"),
        Err("state_mismatch".into())
    );
    assert_eq!(
        parse_callback("code=c1", "s1"),
        Err("state_mismatch".into())
    );
    assert_eq!(
        parse_callback("error=access_denied&state=s1", "s1"),
        Ok(Callback::Denied {
            error: "access_denied".into()
        })
    );
    // A forged or reserved client id is ignored, never used.
    assert_eq!(
        parse_callback("code=c&state=s&client_id=dynamic_agent_client", "s"),
        Ok(Callback::Code {
            code: "c".into(),
            client_id: None
        })
    );
    assert_eq!(
        parse_callback("code=c&state=s&client_id=a%20b%26x", "s"),
        Ok(Callback::Code {
            code: "c".into(),
            client_id: None
        })
    );
}

#[test]
fn id_tokens_are_verified() {
    let iss = Issuer::new();
    let jwks = iss.jwks();
    let ok = iss.token(claims(ISSUER, "cid", "n1", 2_000));
    let acc = verify_id_token(&ok, &jwks, ISSUER, "cid", "n1", 1_000).unwrap();
    assert_eq!(acc.email.as_deref(), Some("ann@example.com"));
    assert_eq!(acc.name.as_deref(), Some("Ann Lee"));
    let cases = [
        (
            iss.token(claims("https://evil.example", "cid", "n1", 2_000)),
            "wrong issuer",
        ),
        (
            iss.token(claims(ISSUER, "other", "n1", 2_000)),
            "wrong audience",
        ),
        (iss.token(claims(ISSUER, "cid", "n2", 2_000)), "wrong nonce"),
        (iss.token(claims(ISSUER, "cid", "n1", 500)), "expired"),
    ];
    for (t, why) in cases {
        assert_eq!(
            verify_id_token(&t, &jwks, ISSUER, "cid", "n1", 1_000),
            Err(why.into())
        );
    }
    // A token signed by another key, or with its claims swapped, fails the signature.
    let other = Issuer::new().token(claims(ISSUER, "cid", "n1", 2_000));
    assert_eq!(
        verify_id_token(&other, &jwks, ISSUER, "cid", "n1", 1_000),
        Err("bad signature".into())
    );
    let mut parts: Vec<&str> = ok.split('.').collect();
    let forged = URL_SAFE_NO_PAD.encode(claims(ISSUER, "cid", "n1", 9_999_999).to_string());
    parts[1] = &forged;
    assert_eq!(
        verify_id_token(&parts.join("."), &jwks, ISSUER, "cid", "n1", 1_000),
        Err("bad signature".into())
    );
    let none = format!(
        "{}.{}.",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
        parts[1]
    );
    assert!(verify_id_token(&none, &jwks, ISSUER, "cid", "n1", 1_000).is_err());
}

#[test]
fn token_responses() {
    let ok = r#"{"access_token": "at", "refresh_token": "rt2", "expires_in": 3600,
                 "scope": "openid offline_access resource.invoke chatgpt.tokens.use.direct", "id_token": "x.y.z"}"#;
    let (t, id) = parse_token_response(200, ok, 100, Some("rt1")).unwrap();
    assert_eq!(
        (
            t.access_token.as_str(),
            t.refresh_token.as_deref(),
            t.expires_at
        ),
        ("at", Some("rt2"), 3700)
    );
    assert!(t.plan_usage());
    assert_eq!(id.as_deref(), Some("x.y.z"));
    let (t, _) = parse_token_response(
        200,
        r#"{"access_token": "at", "scope": "openid"}"#,
        0,
        Some("rt1"),
    )
    .unwrap();
    assert_eq!(
        t.refresh_token.as_deref(),
        Some("rt1"),
        "kept when none rotated in"
    );
    assert!(!t.plan_usage(), "scope not granted");
    for code in REAUTH_CODES {
        assert!(matches!(
            parse_token_response(400, &format!(r#"{{"error": "{code}"}}"#), 0, None),
            Err(TokenError::Reauth(_))
        ));
    }
    assert!(matches!(
        parse_token_response(401, r#"{"error": "invalid_client"}"#, 0, None),
        Err(TokenError::BadClient(_))
    ));
    assert!(matches!(
        parse_token_response(503, "", 0, None),
        Err(TokenError::Transient(_))
    ));
    assert!(matches!(
        parse_token_response(200, "{}", 0, None),
        Err(TokenError::Malformed(_))
    ));
    assert!(!format!("{t:?}").contains("at\""), "Debug hides tokens");
}

/// The whole sign-in against the fake server: exchange, ID token, scopes, then refresh.
#[tokio::test]
async fn sign_in_and_refresh_against_a_fake_server() {
    let server = MockServer::start().await;
    let ep = endpoints(&server);
    let iss = Issuer::new();
    let attempt = Attempt::new("http://127.0.0.1:7777/auth/callback".into()).unwrap();
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(iss.jwks()))
        .mount(&server)
        .await;
    let id_token = iss.token(claims(ISSUER, "cid_42", &attempt.nonce, 10_000));
    Mock::given(method("POST"))
        .and(path("/api/accounts/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("client_id=cid_42"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains(format!("code_verifier={}", attempt.verifier)))
        .and(body_string_contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A7777%2Fauth%2Fcallback"))
        .and(body_string_contains("resource=https%3A%2F%2Fapi.openai.com%2Fv1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600, "token_type": "Bearer",
            "scope": "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
            "id_token": id_token,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let cb = parse_callback(
        &format!("code=the-code&state={}&client_id=cid_42", attempt.state),
        &attempt.state,
    )
    .unwrap();
    let Callback::Code { code, client_id } = cb else {
        panic!()
    };
    let client_id = client_id.unwrap();
    let (tokens, id) = exchange_code(&http, &ep, &client_id, &code, &attempt, 1_000)
        .await
        .unwrap();
    let jwks = fetch_jwks(&http, &ep).await.unwrap();
    let account = verify_id_token(
        &id.unwrap(),
        &jwks,
        &ep.issuer,
        &client_id,
        &attempt.nonce,
        1_000,
    )
    .unwrap();
    assert_eq!(account.sub, "user-1");
    assert!(tokens.plan_usage());

    // Refresh rotates the refresh token; the old one is never sent again.
    Mock::given(method("POST"))
        .and(path("/api/accounts/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=rt-1"))
        .and(body_string_contains("client_id=cid_42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-2", "refresh_token": "rt-2", "expires_in": 3600,
            "scope": "openid offline_access resource.invoke chatgpt.tokens.use.direct"})))
        .expect(1)
        .mount(&server)
        .await;
    let t2 = refresh(
        &http,
        &ep,
        &client_id,
        tokens.refresh_token.as_deref().unwrap(),
        2_000,
    )
    .await
    .unwrap();
    assert_eq!(
        (t2.access_token.as_str(), t2.refresh_token.as_deref()),
        ("at-2", Some("rt-2"))
    );
    // A reused refresh token is terminal: sign in again.
    Mock::given(method("POST"))
        .and(path("/api/accounts/oauth/token"))
        .and(body_string_contains("refresh_token=rt-1"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"error": "refresh_token_reused"})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    assert!(matches!(
        refresh(&http, &ep, &client_id, "rt-1", 3_000).await,
        Err(TokenError::Reauth(_))
    ));
}

#[test]
fn session_sends_a_token_only_with_plan_usage() {
    let clock = Arc::new(AtomicI64::new(1_000));
    let c = clock.clone();
    let s = ChatgptSession::with_clock(Box::new(move || c.load(Ordering::SeqCst)));
    assert!(s.bearer().is_none());
    let mut t = Tokens {
        access_token: "at".into(),
        refresh_token: Some("rt".into()),
        expires_at: 4_600,
        scopes: vec!["openid".into(), "offline_access".into()],
    };
    s.set(t.clone());
    assert!(
        s.bearer().is_none(),
        "no chatgpt.tokens.use.direct, no inference"
    );
    t.scopes.push(PLAN_SCOPE.into());
    s.set(t);
    assert_eq!(
        s.bearer().map(|b| b.expose().to_owned()).as_deref(),
        Some("at")
    );
    assert!(!s.needs_refresh());
    clock.store(4_400, Ordering::SeqCst);
    assert!(s.needs_refresh(), "within the margin");
    clock.store(1_000, Ordering::SeqCst);
    s.rejected();
    assert!(
        s.bearer().is_none() && s.needs_refresh(),
        "a 401 forces a refresh"
    );
    s.set(s.tokens().unwrap());
    s.pause();
    assert!(s.bearer().is_none(), "paused: nothing sent");
    s.resume();
    assert!(s.bearer().is_some());
    s.require_reauth();
    assert!(s.bearer().is_none() && !s.needs_refresh());
    clock.store(10_000, Ordering::SeqCst);
}

fn sse(text: &str) -> String {
    [
        json!({"type": "response.created", "response": {"id": "r1"}}),
        json!({"type": "response.output_text.delta", "delta": text}),
        json!({"type": "response.completed", "response": {"id": "r1", "usage": {"input_tokens": 3, "output_tokens": 2}}}),
    ]
    .iter()
    .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
    .collect()
}

async fn text_of(p: &dyn Provider, req: ChatRequest) -> Result<String> {
    let mut s = p.chat(req).await?;
    let mut out = String::new();
    while let Some(e) = s.next().await {
        if let ChatEvent::TextDelta(t) = e? {
            out.push_str(&t);
        }
    }
    Ok(out)
}

/// A paid provider standing by on the same tier.
struct Paid(ProviderId, Mutex<u32>);
#[async_trait]
impl Provider for Paid {
    fn id(&self) -> &ProviderId {
        &self.0
    }
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: crate::ProviderKind::Anthropic,
            model: "paid".into(),
            locality: Locality::Cloud,
            context_tokens: None,
        }
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
        *self.1.lock().unwrap() += 1;
        Err(Error::Provider("should never be asked".into()))
    }
    async fn embed(&self, _t: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported("no".into()))
    }
}

#[tokio::test]
async fn inference_uses_the_plan_and_stops_at_the_limit_without_falling_back() {
    let server = MockServer::start().await;
    let session = Arc::new(ChatgptSession::new());
    let ps = build_with(
        session.clone(),
        Arc::new(Ledger::default()),
        &format!("{}/v1", server.uri()),
        "gpt-mini",
        "gpt-big",
        &FakeResolver::new(vec![]),
    )
    .unwrap();
    let ask = || ChatRequest::new(vec![Message::user_text("hello")]);

    // Signed in without plan usage: nothing is sent.
    session.set(Tokens {
        access_token: "at".into(),
        refresh_token: Some("rt".into()),
        expires_at: unix_now() + 3600,
        scopes: vec!["openid".into()],
    });
    let e = text_of(ps.smart.as_ref(), ask()).await.unwrap_err();
    assert!(e.to_string().contains(PLAN_SCOPE), "{e}");
    assert!(server.received_requests().await.unwrap().is_empty());

    session.set(Tokens {
        access_token: "at".into(),
        refresh_token: Some("rt".into()),
        expires_at: unix_now() + 3600,
        scopes: SCOPES.split(' ').map(str::to_owned).collect(),
    });
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer at"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse("Hi there")),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    assert_eq!(text_of(ps.smart.as_ref(), ask()).await.unwrap(), "Hi there");
    let body: Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(
        (body["store"].clone(), body["stream"].clone()),
        (json!(false), json!(true))
    );
    assert_eq!(body["model"], "gpt-big");

    // The plan's limit: a clear, non-fallback error, the session pauses, nothing is retried.
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({"error": {
            "code": "subscription_sharing_usage_limit_exceeded", "message": "Usage limit reached", "type": "rate_limit"}})))
        .mount(&server)
        .await;
    let paid = Arc::new(Paid(ProviderId::new("paid"), Mutex::new(0)));
    let mut tiers = TierMap::new();
    tiers.insert(
        Tier::Synthesis,
        vec![ps.smart.id().clone(), paid.id().clone()],
    );
    let router = Router::new(
        vec![ps.smart.clone(), paid.clone() as Arc<dyn Provider>],
        tiers,
    );
    let before = server.received_requests().await.unwrap().len();
    let e = match router
        .chat(Tier::Synthesis, &Default::default(), ask())
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("must not succeed"),
    };
    assert!(is_usage_limit(&e), "{e}");
    assert!(e.to_string().contains(MANAGE_USAGE_URL));
    assert_eq!(
        *paid.1.lock().unwrap(),
        0,
        "never moved to the paid provider"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        before + 1,
        "no retries"
    );
    assert!(session.paused());
    // Paused: fails at once, still without asking the paid provider.
    let e = match router
        .chat(Tier::Synthesis, &Default::default(), ask())
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("must not succeed"),
    };
    assert!(is_usage_limit(&e));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        before + 1,
        "nothing sent while paused"
    );
    assert_eq!(*paid.1.lock().unwrap(), 0);
    // Embeddings never use the plan.
    assert!(ps.light.embed(&["x".into()]).await.is_err());
}

#[tokio::test]
async fn invalid_user_requires_a_new_sign_in() {
    let server = MockServer::start().await;
    let session = Arc::new(ChatgptSession::new());
    let ps = build_with(
        session.clone(),
        Arc::new(Ledger::default()),
        &format!("{}/v1", server.uri()),
        "m",
        "m",
        &FakeResolver::new(vec![]),
    )
    .unwrap();
    session.set(Tokens {
        access_token: "at".into(),
        refresh_token: Some("rt".into()),
        expires_at: unix_now() + 3600,
        scopes: vec![PLAN_SCOPE.into()],
    });
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": {
            "code": "subscription_sharing_invalid_user", "message": "sign in again"}})))
        .mount(&server)
        .await;
    let e = text_of(
        ps.light.as_ref(),
        ChatRequest::new(vec![Message::user_text("x")]),
    )
    .await
    .unwrap_err();
    assert!(is_invalid_user(&e), "{e}");
    assert!(session.needs_reauth());
}

#[test]
fn models_are_picked_from_the_listed_ones() {
    let (all, light, smart) = pick_models(&json!({"data": [
        {"slug": "gpt-6.1-sol", "visibility": "list"},
        {"slug": "internal-x", "visibility": "hide"},
        {"slug": "gpt-6.1-mini", "visibility": "list"},
        {"slug": "bad slug!", "visibility": "list"},
    ]}));
    assert_eq!(all, vec!["gpt-6.1-sol", "gpt-6.1-mini"]);
    assert_eq!(
        (light.as_deref(), smart.as_deref()),
        (Some("gpt-6.1-mini"), Some("gpt-6.1-sol"))
    );
    let (_, light, smart) = pick_models(&json!({"data": [{"id": "only-one"}]}));
    assert_eq!(
        (light.as_deref(), smart.as_deref()),
        (Some("only-one"), Some("only-one"))
    );
    assert_eq!(pick_models(&json!({})).2, None);
}
