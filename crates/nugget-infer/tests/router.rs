use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatResponse, ChatStream, Message, Part, Provider, ProviderConfig,
    ProviderKind, ProviderMeta, ProvidersFile, Router, StopReason, TierMap, tier_map_from_configs,
};

#[derive(Clone, Copy)]
enum Behaviour {
    Ok,
    FailBeforeStream,
    FailFirstEvent,
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
            Behaviour::InvalidRequest => Err(Error::Invalid("bad request".into())),
        }
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.behaviour {
            Behaviour::Ok => Ok(texts.iter().map(|_| vec![1.0]).collect()),
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

#[test]
fn routes_in_order_with_capability_filtering() {
    let a = Fake::new("a", Behaviour::Ok);
    let b = Fake::new("b", Behaviour::Ok);
    let router = Router::new(
        vec![a, b],
        tiers(&[(Tier::Synthesis, &["a", "b"]), (Tier::Light, &["b"])]),
    );
    router.set_capabilities(&ProviderId::new("a"), caps(false, true, false));
    router.set_capabilities(&ProviderId::new("b"), caps(true, true, false));

    let none = Capabilities::default();
    assert_eq!(
        router.route(Tier::Synthesis, &none).unwrap().id().as_str(),
        "a"
    );
    let vision = caps(true, false, false);
    assert_eq!(
        router
            .route(Tier::Synthesis, &vision)
            .unwrap()
            .id()
            .as_str(),
        "b"
    );
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
        router.tiers_of(&ProviderId::new("b")),
        vec![Tier::Light, Tier::Synthesis]
    );
}

#[test]
fn unprobed_providers_only_serve_plain_chat() {
    let router = Router::new(
        vec![Fake::new("a", Behaviour::Ok)],
        tiers(&[(Tier::Light, &["a"])]),
    );
    assert!(router.route(Tier::Light, &Capabilities::default()).is_ok());
    assert!(
        router
            .route(Tier::Light, &caps(false, true, false))
            .is_err()
    );
    let ctx = Capabilities {
        context_tokens: Some(8000),
        ..Default::default()
    };
    assert!(router.route(Tier::Light, &ctx).is_err());
}

#[test]
fn failed_provider_cools_down() {
    let router = Router::new(
        vec![Fake::new("a", Behaviour::Ok), Fake::new("b", Behaviour::Ok)],
        tiers(&[(Tier::Light, &["a", "b"])]),
    )
    .with_cooldown(Duration::from_millis(50));
    let none = Capabilities::default();
    router.report_failure(&ProviderId::new("a"));
    assert_eq!(ids(&router.candidates(Tier::Light, &none)), vec!["b", "a"]);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(ids(&router.candidates(Tier::Light, &none)), vec!["a", "b"]);
    router.report_failure(&ProviderId::new("a"));
    router.report_success(&ProviderId::new("a"));
    assert_eq!(router.route(Tier::Light, &none).unwrap().id().as_str(), "a");
}

#[tokio::test]
async fn chat_falls_back_on_early_failures() {
    let a = Fake::new("a", Behaviour::FailBeforeStream);
    let b = Fake::new("b", Behaviour::FailFirstEvent);
    let c = Fake::new("c", Behaviour::Ok);
    let router = Router::new(
        vec![a.clone(), b.clone(), c.clone()],
        tiers(&[(Tier::Synthesis, &["a", "b", "c"])]),
    );
    let (p, stream) = router
        .chat(
            Tier::Synthesis,
            &Capabilities::default(),
            ChatRequest::new(vec![Message::user_text("hi")]),
        )
        .await
        .unwrap();
    assert_eq!(p.id().as_str(), "c");
    let r = ChatResponse::collect(stream).await.unwrap();
    assert_eq!(r.text(), "from c", "first event is not lost");
    assert_eq!(
        (
            a.calls.load(Ordering::SeqCst),
            b.calls.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
    // Failed providers are now tried last.
    let order = ids(&router.candidates(Tier::Synthesis, &Capabilities::default()));
    assert_eq!(order, vec!["c", "a", "b"]);
}

#[tokio::test]
async fn chat_does_not_fall_back_on_invalid_requests() {
    let a = Fake::new("a", Behaviour::InvalidRequest);
    let b = Fake::new("b", Behaviour::Ok);
    let router = Router::new(vec![a, b.clone()], tiers(&[(Tier::Light, &["a", "b"])]));
    let err = router
        .chat(
            Tier::Light,
            &Capabilities::default(),
            ChatRequest::new(vec![Message::user_text("hi")]),
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::Invalid(_)));
    assert_eq!(b.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn chat_requires_capabilities_implied_by_request() {
    let a = Fake::new("a", Behaviour::Ok);
    let b = Fake::new("b", Behaviour::Ok);
    let router = Router::new(vec![a, b], tiers(&[(Tier::Agentic, &["a", "b"])]));
    router.set_capabilities(&ProviderId::new("a"), caps(false, true, false));
    router.set_capabilities(&ProviderId::new("b"), caps(true, true, false));
    let req = ChatRequest::new(vec![Message::user(vec![
        Part::image("image/png", "AAAA"),
        Part::text("what is this"),
    ])]);
    let (p, _) = router
        .chat(Tier::Agentic, &Capabilities::default(), req)
        .await
        .unwrap();
    assert_eq!(p.id().as_str(), "b");

    let all_fail = Router::new(
        vec![Fake::new("x", Behaviour::FailBeforeStream)],
        tiers(&[(Tier::Light, &["x"])]),
    );
    let err = all_fail
        .chat(
            Tier::Light,
            &Capabilities::default(),
            ChatRequest::new(vec![Message::user_text("hi")]),
        )
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("HTTP 503"));
}

#[tokio::test]
async fn embed_uses_embedding_tier_with_fallback() {
    let a = Fake::new("a", Behaviour::FailBeforeStream);
    let b = Fake::new("b", Behaviour::Ok);
    let router = Router::new(vec![a, b], tiers(&[(Tier::Embedding, &["a", "b"])]));
    assert!(
        router.embed(&["x".into()]).await.is_err(),
        "unprobed: no embeddings proven"
    );
    router.set_capabilities(&ProviderId::new("a"), caps(false, false, true));
    router.set_capabilities(&ProviderId::new("b"), caps(false, false, true));
    let (id, v) = router.embed(&["x".into(), "y".into()]).await.unwrap();
    assert_eq!(id.as_str(), "b");
    assert_eq!(v.len(), 2);
}

#[test]
fn tier_map_follows_config_order() {
    let file = ProvidersFile::from_toml_str(
        r#"
        [[providers]]
        id = "local"
        kind = "openai_compatible"
        base_url = "http://localhost:11434/v1"
        model = "m"
        tiers = ["triage", "light"]

        [[providers]]
        id = "cloud"
        kind = "anthropic"
        model = "m"
        tiers = ["light", "synthesis"]
        "#,
    )
    .unwrap();
    let map = tier_map_from_configs(&file.providers);
    let get = |t| {
        map.get(&t)
            .unwrap()
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(get(Tier::Light), vec!["local", "cloud"]);
    assert_eq!(get(Tier::Triage), vec!["local"]);
    assert_eq!(get(Tier::Synthesis), vec!["cloud"]);
    let _: &ProviderConfig = &file.providers[0];
}
