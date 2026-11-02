//! The Heed cloud provider against a test double of the endpoint (wiremock).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use futures::StreamExt;
use nugget_core::egress::EgressEvent;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::locality::tests::FakeResolver;
use crate::router::Router;
use crate::types::{ChatEvent, Message};

#[derive(Default)]
struct Ledger(Mutex<Vec<EgressEvent>>);
impl EgressSink for Ledger {
    fn record(&self, e: EgressEvent) {
        self.0.lock().unwrap().push(e);
    }
}

fn sse(text: &str) -> String {
    let events = [
        (
            "message_start",
            json!({"type": "message_start", "message": {"id": "m", "type": "message", "role": "assistant",
                "model": "heed-fast", "content": [], "usage": {"input_tokens": 9, "output_tokens": 0}}}),
        ),
        (
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        (
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        ),
        (
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        ),
        (
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        ),
        ("message_stop", json!({"type": "message_stop"})),
    ];
    events
        .iter()
        .map(|(n, d)| format!("event: {n}\ndata: {d}\n\n"))
        .collect()
}

fn reply(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse(text))
}

struct Fixture {
    server: MockServer,
    session: Arc<CloudSession>,
    clock: Arc<AtomicI64>,
    ledger: Arc<Ledger>,
    providers: CloudProviders,
}

async fn fixture() -> Fixture {
    let server = MockServer::start().await;
    let clock = Arc::new(AtomicI64::new(1_000));
    let c = clock.clone();
    let session = Arc::new(CloudSession::with_clock(Box::new(move || {
        c.load(Ordering::SeqCst)
    })));
    let ledger = Arc::new(Ledger::default());
    let providers = build_with(
        &server.uri(),
        session.clone(),
        ledger.clone(),
        &FakeResolver::new(vec![]),
    )
    .unwrap();
    Fixture {
        server,
        session,
        clock,
        ledger,
        providers,
    }
}

fn ask(text: &str) -> ChatRequest {
    ChatRequest::new(vec![Message::user_text(text)])
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

fn bodies(reqs: &[wiremock::Request]) -> Vec<Value> {
    reqs.iter().map(|r| r.body_json().unwrap()).collect()
}

#[test]
fn base_url_rules() {
    assert_eq!(
        llm_base_url("https://api.heed.example/").unwrap(),
        "https://api.heed.example/llm"
    );
    assert_eq!(
        llm_base_url("http://127.0.0.1:8080").unwrap(),
        "http://127.0.0.1:8080/llm"
    );
    for bad in [
        "http://api.heed.example",
        "ftp://x",
        "https://u:p@api.heed.example",
        "https://api.heed.example/?a=1",
        "not a url",
    ] {
        assert!(llm_base_url(bad).is_err(), "{bad}");
    }
}

#[test]
fn tokens_expire_early_and_clear() {
    let now = Arc::new(AtomicI64::new(100));
    let n = now.clone();
    let s = CloudSession::with_clock(Box::new(move || n.load(Ordering::SeqCst)));
    assert!(!s.is_active());
    s.set(Secret::new("t"), 100 + EXPIRY_MARGIN_SECS + 1);
    assert_eq!(s.token().unwrap().expose(), "t");
    now.store(101, Ordering::SeqCst);
    assert!(s.token().is_none(), "inside the margin counts as expired");
    assert_eq!(s.expires_at(), Some(116));
    s.set(Secret::new("t2"), 10_000);
    s.rejected();
    assert!(s.token().is_none() && s.expires_at().is_none());
    assert!(!format!("{s:?}").contains("t2"));
}

/// Both tiers go to `{api}/llm/v1/messages` with the session's current token, as Cloud
/// egress; a refreshed token is used without rebuilding anything.
#[tokio::test]
async fn sends_the_current_token_and_tier_and_records_egress() {
    let f = fixture().await;
    Mock::given(method("POST"))
        .and(path("/llm/v1/messages"))
        .respond_with(reply("Hello"))
        .mount(&f.server)
        .await;
    assert!(!f.providers.fast.available());
    f.session.set(Secret::new("tok-1"), 5_000);
    assert!(f.providers.fast.available() && f.providers.smart.available());
    assert_eq!(f.providers.fast.meta().locality, Locality::Cloud);
    assert_eq!(f.providers.fast.id().as_str(), CLOUD_FAST);
    assert_eq!(f.providers.smart.id().as_str(), CLOUD_SMART);

    let fast = f.providers.fast.as_ref();
    assert_eq!(text_of(fast, ask("hi")).await.unwrap(), "Hello");
    f.session.set(Secret::new("tok-2"), 9_000);
    let smart = f.providers.smart.as_ref();
    assert_eq!(text_of(smart, ask("hi")).await.unwrap(), "Hello");

    let reqs = f.server.received_requests().await.unwrap();
    let auth: Vec<_> = reqs
        .iter()
        .map(|r| r.headers["authorization"].to_str().unwrap().to_owned())
        .collect();
    assert_eq!(auth, ["Bearer tok-1", "Bearer tok-2"]);
    let b = bodies(&reqs);
    assert_eq!(b[0]["model"], FAST_MODEL);
    assert_eq!(b[1]["model"], SMART_MODEL);
    assert_eq!(b[0]["stream"], true);
    assert!(reqs.iter().all(|r| r.headers.get("x-api-key").is_none()));

    let ledger = f.ledger.0.lock().unwrap();
    assert!(
        !ledger.is_empty(),
        "cloud requests are in the egress ledger"
    );
    let host = f.server.address().to_string();
    assert!(
        ledger.iter().all(|e| format!("{e:?}").contains(&host)),
        "{ledger:?}"
    );
}

/// No token, an expired token or a refused token: nothing is sent (no ledger row), the provider
/// is unavailable, and the error is one the router falls back on.
#[tokio::test]
async fn without_a_valid_token_nothing_leaves() {
    let f = fixture().await;
    Mock::given(method("POST"))
        .and(path("/llm/v1/messages"))
        .and(header("authorization", "Bearer stale"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error", "error": {"type": "authentication_error", "message": "sign in"}
        })))
        .mount(&f.server)
        .await;
    let fast = f.providers.fast.as_ref();
    let err = text_of(fast, ask("secret")).await.unwrap_err();
    assert!(matches!(err, Error::Provider(_)), "{err:?}");
    f.session.set(Secret::new("old"), 1_010);
    assert!(!fast.available(), "expires within the margin");
    assert!(text_of(fast, ask("secret")).await.is_err());
    assert!(f.server.received_requests().await.unwrap().is_empty());
    assert!(f.ledger.0.lock().unwrap().is_empty());
    fast.prewarm().await.unwrap();
    assert!(
        f.server.received_requests().await.unwrap().is_empty(),
        "no prewarm while signed out"
    );

    // The server refuses the token: it is dropped until a fresh one arrives.
    f.session.set(Secret::new("stale"), 5_000);
    assert!(text_of(fast, ask("x")).await.is_err());
    assert!(!fast.available());
    assert_eq!(f.server.received_requests().await.unwrap().len(), 1);
    f.clock.store(1_001, Ordering::SeqCst);
    f.session.set(Secret::new("fresh"), 5_000);
    assert!(fast.available());
}

/// Prewarm is one authenticated GET of `{api}/llm/v1/models`, on the pool both tiers share.
#[tokio::test]
async fn prewarm_opens_the_shared_connection() {
    let f = fixture().await;
    Mock::given(method("GET"))
        .and(path("/llm/v1/models"))
        .and(header("authorization", "Bearer t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect(1)
        .mount(&f.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/llm/v1/messages"))
        .respond_with(reply("ok"))
        .mount(&f.server)
        .await;
    f.session.set(Secret::new("t"), 5_000);
    f.providers.fast.prewarm().await.unwrap();
    text_of(f.providers.smart.as_ref(), ask("q")).await.unwrap();
    // wiremock does not expose connections; the shared pool is asserted by construction
    // (`Http::sibling`) and measured in nugget-cloud's end-to-end latency test.
    f.server.verify().await;
}

/// A local provider that answers "local".
struct Local(ProviderId);

#[async_trait]
impl Provider for Local {
    fn id(&self) -> &ProviderId {
        &self.0
    }
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: crate::types::ProviderKind::OpenaiCompatible,
            model: "small".into(),
            locality: Locality::OnDevice,
            context_tokens: None,
        }
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ChatEvent::TextDelta("local".into())),
            Ok(ChatEvent::Done {
                stop_reason: crate::types::StopReason::EndTurn,
                input_tokens: None,
                output_tokens: None,
            }),
        ])))
    }
    async fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported("no".into()))
    }
}

async fn routed(r: &Router, tier: Tier) -> (String, String) {
    let (p, mut s) = r
        .chat(tier, &Capabilities::default(), ask("q"))
        .await
        .unwrap();
    let mut out = String::new();
    while let Some(e) = s.next().await {
        if let Ok(ChatEvent::TextDelta(t)) = e {
            out.push_str(&t);
        }
    }
    (p.id().to_string(), out)
}

/// Signed in, the cloud serves first; a quota answer (429) or a lost token falls back to the
/// local model at once.
#[tokio::test]
async fn router_prefers_cloud_and_falls_back_to_local() {
    let f = fixture().await;
    let local: Arc<dyn Provider> = Arc::new(Local(ProviderId::new("local-chat")));
    let mut tiers: HashMap<Tier, Vec<ProviderId>> = HashMap::new();
    for t in [Tier::Light, Tier::Synthesis] {
        tiers.insert(t, vec![local.id().clone()]);
    }
    let router = Router::new(vec![local.clone()], tiers);
    for (p, t) in [
        (&f.providers.fast, FAST_TIERS),
        (&f.providers.smart, SMART_TIERS),
    ] {
        router.set_provider_before(p.clone(), t, &|_| true);
        router.set_capabilities(p.id(), cloud_capabilities());
    }
    f.session.set(Secret::new("t"), 5_000);
    Mock::given(method("POST"))
        .and(path("/llm/v1/messages"))
        .respond_with(reply("cloud"))
        .up_to_n_times(2)
        .mount(&f.server)
        .await;
    assert_eq!(
        routed(&router, Tier::Light).await,
        (CLOUD_FAST.to_owned(), "cloud".to_owned())
    );
    assert_eq!(
        routed(&router, Tier::Synthesis).await,
        (CLOUD_SMART.to_owned(), "cloud".to_owned())
    );

    Mock::given(method("POST"))
        .and(path("/llm/v1/messages"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "type": "error", "error": {"type": "rate_limit_error", "message": "monthly AI quota used up", "code": "quota_exceeded"}
        })))
        .mount(&f.server)
        .await;
    let t0 = std::time::Instant::now();
    assert_eq!(
        routed(&router, Tier::Light).await,
        ("local-chat".to_owned(), "local".to_owned())
    );
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(1),
        "no retry delay"
    );

    f.session.clear();
    assert_eq!(routed(&router, Tier::Synthesis).await.0, "local-chat");
}
