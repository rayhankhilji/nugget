use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatResponse, ChatStream, Message, Part, Provider, ProviderKind,
    ProviderMeta, Router, StopReason, TierMap,
};

#[derive(Clone, Copy)]
enum Behaviour {
    Ok,
    FailBeforeStream,
    FailFirstEvent,
    RateLimited,
    NetworkDown,
    InvalidRequest,
}

struct Fake {
    id: ProviderId,
    behaviour: Behaviour,
    calls: AtomicUsize,
}

impl Fake {
    fn new(id: &str, behaviour: Behaviour) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId::new(id),
            behaviour,
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl Provider for Fake {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "fake".into(),
            locality: Locality::OnDevice,
            context_tokens: None,
        }
    }

    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.behaviour {
            Behaviour::Ok => Ok(Box::pin(stream::iter(vec![
                Ok(ChatEvent::TextDelta(format!("from {}", self.id))),
                Ok(ChatEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    input_tokens: None,
                    output_tokens: None,
                }),
            ]))),
            Behaviour::FailBeforeStream => Err(Error::Provider("HTTP 503".into())),
            Behaviour::FailFirstEvent => Ok(Box::pin(stream::iter(vec![Err(Error::Provider(
                "overloaded".into(),
            ))]))),
            Behaviour::RateLimited => Err(Error::RateLimited {
                retry_after_ms: Some(1000),
            }),
            Behaviour::NetworkDown => Err(Error::Network("connection failed".into())),
            Behaviour::InvalidRequest => Err(Error::Invalid("bad request".into())),
        }
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.behaviour {
            Behaviour::Ok => Ok(texts.iter().map(|_| vec![1.0]).collect()),
            Behaviour::RateLimited => Err(Error::RateLimited {
                retry_after_ms: None,
            }),
            Behaviour::NetworkDown => Err(Error::Network("timed out".into())),
            _ => Err(Error::Provider("down".into())),
        }
    }
}

fn caps(vision: bool, tools: bool, embeddings: bool) -> Capabilities {
    Capabilities {
        streaming: true,
        vision,
        tools,
        json_schema: false,
        embeddings,
        context_tokens: None,
    }
}

fn tiers(entries: &[(Tier, &[&str])]) -> TierMap {
    entries
        .iter()
        .map(|(t, ids)| (*t, ids.iter().map(|i| ProviderId::new(*i)).collect()))
        .collect()
}

fn ids(ps: &[Arc<dyn Provider>]) -> Vec<String> {
    ps.iter().map(|p| p.id().to_string()).collect()
}

fn id(s: &str) -> ProviderId {
    ProviderId::new(s)
}

fn hi() -> ChatRequest {
    ChatRequest::new(vec![Message::user_text("hi")])
}

#[test]
fn routes_in_order_with_capability_filtering_and_cooldown() {
    let router = Router::new(
        vec![Fake::new("a", Behaviour::Ok), Fake::new("b", Behaviour::Ok)],
        tiers(&[(Tier::Synthesis, &["a", "b"]), (Tier::Light, &["b"])]),
    )
    .with_cooldown(Duration::from_millis(50));
    let none = Capabilities::default();
    // Unprobed providers only serve plain chat.
    assert!(
        router
            .route(Tier::Synthesis, &caps(false, true, false))
            .is_err()
    );
    let ctx = Capabilities {
        context_tokens: Some(8000),
        ..Default::default()
    };
    assert!(router.route(Tier::Synthesis, &ctx).is_err());

    router.set_capabilities(&id("a"), caps(false, true, false));
    router.set_capabilities(&id("b"), caps(true, true, false));
    assert_eq!(
        router.route(Tier::Synthesis, &none).unwrap().id().as_str(),
        "a"
    );
    let vision = caps(true, false, false);
    assert_eq!(ids(&router.candidates(Tier::Synthesis, &vision)), vec!["b"]);
    let embeddings = caps(false, false, true);
    assert!(matches!(
        router.route(Tier::Synthesis, &embeddings),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        router.route(Tier::Agentic, &none),
        Err(Error::NotFound(_))
    ));
    assert_eq!(
        router.tiers_of(&id("b")),
        vec![Tier::Light, Tier::Synthesis]
    );

    // A failed provider is tried last until its cooldown passes or it succeeds.
    router.report_failure(&id("a"));
    assert_eq!(
        ids(&router.candidates(Tier::Synthesis, &none)),
        vec!["b", "a"]
    );
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(
        ids(&router.candidates(Tier::Synthesis, &none)),
        vec!["a", "b"]
    );
    router.report_failure(&id("a"));
    router.report_success(&id("a"));
    assert_eq!(
        router.route(Tier::Synthesis, &none).unwrap().id().as_str(),
        "a"
    );
}

#[tokio::test]
async fn chat_falls_back_on_early_and_transient_failures() {
    let fakes = [
        Fake::new("a", Behaviour::FailBeforeStream),
        Fake::new("b", Behaviour::FailFirstEvent),
        Fake::new("c", Behaviour::RateLimited),
        Fake::new("d", Behaviour::NetworkDown),
        Fake::new("e", Behaviour::Ok),
    ];
    let providers: Vec<Arc<dyn Provider>> = fakes.iter().map(|f| f.clone() as _).collect();
    let router = Router::new(
        providers,
        tiers(&[(Tier::Light, &["a", "b", "c", "d", "e"])]),
    );
    let (p, stream) = router
        .chat(Tier::Light, &Capabilities::default(), hi())
        .await
        .unwrap();
    assert_eq!(p.id().as_str(), "e");
    assert_eq!(
        ChatResponse::collect(stream).await.unwrap().text(),
        "from e",
        "first event kept"
    );
    assert!(fakes.iter().all(|f| f.calls.load(Ordering::SeqCst) == 1));
    let order = ids(&router.candidates(Tier::Light, &Capabilities::default()));
    assert_eq!(
        order,
        vec!["e", "a", "b", "c", "d"],
        "failed providers cool down"
    );

    // When every candidate fails, the caller sees the last error (with a rate limit's hint).
    for (b, check) in [
        (
            Behaviour::RateLimited,
            (|e: &Error| {
                matches!(
                    e,
                    Error::RateLimited {
                        retry_after_ms: Some(1000)
                    }
                )
            }) as fn(&Error) -> bool,
        ),
        (Behaviour::FailBeforeStream, |e| {
            e.to_string().contains("HTTP 503")
        }),
    ] {
        let router = Router::new(vec![Fake::new("x", b)], tiers(&[(Tier::Light, &["x"])]));
        let err = router
            .chat(Tier::Light, &Capabilities::default(), hi())
            .await
            .err()
            .unwrap();
        assert!(check(&err), "{err}");
    }

    // Invalid requests are not retried elsewhere.
    let ok = Fake::new("b", Behaviour::Ok);
    let router = Router::new(
        vec![Fake::new("a", Behaviour::InvalidRequest), ok.clone()],
        tiers(&[(Tier::Light, &["a", "b"])]),
    );
    let err = router
        .chat(Tier::Light, &Capabilities::default(), hi())
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::Invalid(_)));
    assert_eq!(ok.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn chat_requires_capabilities_implied_by_request() {
    let router = Router::new(
        vec![Fake::new("a", Behaviour::Ok), Fake::new("b", Behaviour::Ok)],
        tiers(&[(Tier::Agentic, &["a", "b"])]),
    );
    router.set_capabilities(&id("a"), caps(false, true, false));
    router.set_capabilities(&id("b"), caps(true, true, false));
    let req = ChatRequest::new(vec![Message::user(vec![
        Part::image("image/png", "AAAA"),
        Part::text("what is this"),
    ])]);
    let (p, _) = router
        .chat(Tier::Agentic, &Capabilities::default(), req)
        .await
        .unwrap();
    assert_eq!(p.id().as_str(), "b");
}

#[tokio::test]
async fn embed_uses_embedding_tier_with_fallback() {
    let router = Router::new(
        vec![
            Fake::new("a", Behaviour::FailBeforeStream),
            Fake::new("b", Behaviour::RateLimited),
            Fake::new("c", Behaviour::NetworkDown),
            Fake::new("d", Behaviour::Ok),
        ],
        tiers(&[(Tier::Embedding, &["a", "b", "c", "d"])]),
    );
    assert!(
        router.embed(&["x".into()]).await.is_err(),
        "unprobed: no embeddings proven"
    );
    for p in ["a", "b", "c", "d"] {
        router.set_capabilities(&id(p), caps(false, false, true));
    }
    let (used, v) = router.embed(&["x".into(), "y".into()]).await.unwrap();
    assert_eq!((used.as_str(), v.len()), ("d", 2));
}

#[test]
fn providers_can_be_added_replaced_and_removed_while_in_use() {
    let router = Router::new(
        vec![Fake::new("a", Behaviour::Ok)],
        tiers(&[(Tier::Light, &["a"])]),
    );
    let none = Capabilities::default();
    assert!(router.route(Tier::Synthesis, &none).is_err());

    // A new id joins its tiers after the providers already there.
    router.set_provider(
        Fake::new("b", Behaviour::Ok),
        &[Tier::Light, Tier::Synthesis],
    );
    assert_eq!(ids(&router.candidates(Tier::Light, &none)), ["a", "b"]);
    assert_eq!(
        router.route(Tier::Synthesis, &none).unwrap().id().as_str(),
        "b"
    );
    assert_eq!(router.tiers_of(&id("b")), [Tier::Light, Tier::Synthesis]);
    assert_eq!(ids(&router.providers()), ["a", "b"]);

    // Replacing keeps the place and drops the cooldown and the probed capabilities.
    router.set_capabilities(&id("a"), caps(true, true, false));
    router.report_failure(&id("a"));
    let a2 = Fake::new("a", Behaviour::Ok);
    router.set_provider(a2.clone(), &[Tier::Light]);
    assert!(!router.in_cooldown(&id("a")));
    assert!(router.capabilities(&id("a")).is_none());
    assert_eq!(ids(&router.candidates(Tier::Light, &none)), ["a", "b"]);
    assert!(Arc::ptr_eq(
        &router.provider(&id("a")).unwrap(),
        &(a2 as Arc<dyn Provider>)
    ));

    // Removing stops routing to it; it can come back.
    assert!(router.remove_provider(&id("b")));
    assert!(!router.remove_provider(&id("b")));
    assert!(matches!(
        router.route(Tier::Synthesis, &none),
        Err(Error::NotFound(_))
    ));
    assert!(router.remove_provider(&id("a")));
    assert!(router.providers().is_empty());
    router.set_provider(Fake::new("a", Behaviour::Ok), &[Tier::Light]);
    assert_eq!(ids(&router.candidates(Tier::Light, &none)), ["a"]);

    // `set_provider_before` places a new id before the first match; fewer tiers on replace
    // leaves the others.
    router.set_provider(Fake::new("cloud", Behaviour::Ok), &[Tier::Light]);
    router.set_provider_before(
        Fake::new("lan", Behaviour::Ok),
        &[Tier::Light, Tier::Synthesis],
        &|i| i.as_str() == "cloud",
    );
    assert_eq!(
        ids(&router.candidates(Tier::Light, &none)),
        ["a", "lan", "cloud"]
    );
    router.set_provider(Fake::new("lan", Behaviour::Ok), &[Tier::Light]);
    assert_eq!(router.tiers_of(&id("lan")), [Tier::Light]);
    assert_eq!(
        ids(&router.candidates(Tier::Light, &none)),
        ["a", "lan", "cloud"]
    );
}
