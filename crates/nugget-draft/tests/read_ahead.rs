//! Read-ahead (`Drafter::read_ahead`) and small-model routing (`LocalBudget::provider`): scripted
//! providers, a recording warmer, an in-memory store. No network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nugget_core::capability::{Locality, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{AppInfo, DraftContext, DraftRequest, FieldInfo, ThreadMessage};
use nugget_core::{Error, Result};
use nugget_draft::{
    DraftConfig, Drafter, FEATURE_DRAFT, LocalBudget, NoVoice, ReadAheadConfig, ReadAheadSkip,
    Refusal, Warmer,
};
use nugget_infer::{
    CacheHint, ChatEvent, ChatRequest, ChatStream, Message, Part, Provider, ProviderKind,
    ProviderMeta, Router, StopReason,
};
use nugget_retrieve::{RetrieveConfig, Retriever};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use tokio::sync::Notify;

struct Fake {
    id: ProviderId,
    locality: Locality,
    fail: bool,
    requests: Mutex<Vec<ChatRequest>>,
}

impl Fake {
    fn new(id: &str, locality: Locality) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId::new(id),
            locality,
            fail: false,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn failing(id: &str, locality: Locality) -> Arc<Self> {
        Arc::new(Self {
            fail: true,
            ..Arc::into_inner(Self::new(id, locality)).unwrap()
        })
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
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
            locality: self.locality,
            context_tokens: None,
        }
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        self.requests.lock().unwrap().push(req);
        if self.fail {
            return Err(Error::Network("connection refused".into()));
        }
        let events: Vec<Result<ChatEvent>> = vec![
            Ok(ChatEvent::TextDelta("ok".into())),
            Ok(ChatEvent::Done {
                stop_reason: StopReason::EndTurn,
                input_tokens: Some(10),
                output_tokens: Some(1),
            }),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported("no embeddings".into()))
    }
}

/// Records warms; with `hold`, each prefix warm waits until released (a slow prefill).
#[derive(Default)]
struct Rec {
    warms: Mutex<Vec<ProviderId>>,
    prefixes: Mutex<Vec<(ProviderId, ChatRequest)>>,
    hold: bool,
    release: Notify,
}

impl Rec {
    fn holding() -> Arc<Self> {
        Arc::new(Self {
            hold: true,
            ..Default::default()
        })
    }

    fn warms(&self) -> usize {
        self.warms.lock().unwrap().len()
    }

    fn prefixes(&self) -> Vec<(ProviderId, ChatRequest)> {
        self.prefixes.lock().unwrap().clone()
    }
}

#[async_trait]
impl Warmer for Rec {
    async fn warm(&self, provider: Arc<dyn Provider>) {
        self.warms.lock().unwrap().push(provider.id().clone());
    }

    async fn warm_prefix(&self, provider: Arc<dyn Provider>, prefix: ChatRequest) {
        self.prefixes
            .lock()
            .unwrap()
            .push((provider.id().clone(), prefix));
        if self.hold {
            self.release.notified().await;
        }
    }
}

fn seeded() -> Store {
    let store = Store::open_in_memory().unwrap();
    for (key, title, text) in [
        (
            "q3",
            "Q3 revenue",
            "Q3 revenue numbers came in at 4.2 million, up 12 percent.",
        ),
        (
            "garden",
            "Garden",
            "Tomatoes need watering twice a week in summer.",
        ),
    ] {
        let item = NewItem::new("notes", key, content_hash(text))
            .with_title(title)
            .with_metadata(serde_json::json!({ "sensitive": false }));
        store.upsert_item(&item).unwrap();
        store
            .replace_chunks(&item.item, &[NewChunk::text(text)])
            .unwrap();
    }
    store
}

/// A drafter over `providers` (all known to the router) with `tier` as the Synthesis tier.
fn drafter(
    providers: Vec<Arc<Fake>>,
    tier: &[&str],
    config: DraftConfig,
    warmer: Option<Arc<Rec>>,
) -> Drafter {
    let store = seeded();
    let dyn_providers: Vec<Arc<dyn Provider>> = providers
        .into_iter()
        .map(|p| p as Arc<dyn Provider>)
        .collect();
    let router = Arc::new(Router::new(
        dyn_providers,
        HashMap::from([(
            Tier::Synthesis,
            tier.iter().map(|id| ProviderId::new(*id)).collect(),
        )]),
    ));
    let retriever = Retriever::new(store.clone(), None, RetrieveConfig::default());
    let d = Drafter::new(store, retriever, router, Arc::new(NoVoice), config);
    match warmer {
        Some(w) => d.with_warmer(w),
        None => d,
    }
}

fn no_rate_limit() -> DraftConfig {
    DraftConfig {
        read_ahead: ReadAheadConfig {
            enabled: true,
            min_interval_ms: 0,
        },
        ..Default::default()
    }
}

fn slack_ctx(message: &str) -> DraftContext {
    DraftContext {
        app: AppInfo {
            id: Some("com.tinyspeck.slackmacgap".into()),
            name: "Slack".into(),
            ..Default::default()
        },
        field: FieldInfo {
            role: "text_area".into(),
            placeholder: Some("Message #finance".into()),
            ..Default::default()
        },
        thread: vec![ThreadMessage {
            author: Some("Sam".into()),
            text: message.into(),
            from_me: Some(false),
            timestamp: None,
        }],
        ..Default::default()
    }
}

fn mail_ctx(message: &str) -> DraftContext {
    let mut ctx = slack_ctx(message);
    ctx.app = AppInfo {
        id: Some("com.apple.mail".into()),
        name: "Mail".into(),
        ..Default::default()
    };
    ctx.field.placeholder = None;
    ctx.subject = Some("Q3".into());
    ctx
}

fn request(ctx: DraftContext) -> DraftRequest {
    DraftRequest {
        context: ctx,
        instruction: None,
        provider: None,
        max_tokens: None,
    }
}

fn without_breakpoints(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .map(|m| {
            Message::new(
                m.role,
                m.content
                    .iter()
                    .filter(|p| !matches!(p, Part::CacheBreakpoint))
                    .cloned()
                    .collect(),
            )
        })
        .collect()
}

/// Wait (bounded) until `f` holds.
async fn until(mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not met in time");
}

const ASK: &str = "Can you send the Q3 numbers?";

#[tokio::test]
async fn read_ahead_prefills_exactly_what_begin_sends() {
    let local = Fake::new("local", Locality::OnDevice);
    let rec = Arc::new(Rec::default());
    let d = drafter(
        vec![local.clone()],
        &["local"],
        DraftConfig::default(),
        Some(rec.clone()),
    );
    let out = d.read_ahead(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(out.provider.as_str(), "local");
    assert!(out.prompt_chars > 100, "{out:?}");
    assert!(local.requests().is_empty(), "a prefill, not a chat");
    let prefixes = rec.prefixes();
    assert_eq!(prefixes.len(), 1);
    let (id, warm) = prefixes[0].clone();
    assert_eq!(id.as_str(), "local");

    d.begin(request(slack_ctx(ASK))).await.unwrap();
    let sent = local.requests().pop().unwrap();
    assert_eq!(warm.system, sent.system);
    assert_eq!(without_breakpoints(&warm.messages), sent.messages);
    assert_eq!(warm.cache, sent.cache);
    assert_eq!(warm.cache, Some(CacheHint::keyed("draft:chat")));
    assert_eq!(warm.feature, FEATURE_DRAFT);
    assert_eq!(warm.items, sent.items);
    assert!(sent.items.iter().any(|i| i.key.as_str() == "q3"));
    // What a prefix warm evaluates (llama-server `n_predict: 0`) is the whole draft prompt.
    let evaluated = warm.cache_prefix().unwrap();
    assert_eq!(evaluated.system, sent.system);
    assert_eq!(without_breakpoints(&evaluated.messages), sent.messages);
    // The trigger's prewarm did not run here, and nothing else was warmed.
    assert_eq!(rec.prefixes().len(), 1);
    assert_eq!(rec.warms(), 0);
}

#[tokio::test]
async fn nothing_leaves_for_a_cloud_provider() {
    let cloud = Fake::new("cloud", Locality::Cloud);
    let rec = Arc::new(Rec::default());
    let d = drafter(
        vec![cloud.clone()],
        &["cloud"],
        no_rate_limit(),
        Some(rec.clone()),
    );
    assert_eq!(
        d.read_ahead(request(slack_ctx(ASK))).await,
        Err(ReadAheadSkip::Cloud)
    );
    // An explicit cloud choice is no different.
    let local = Fake::new("local", Locality::OnDevice);
    let d = drafter(
        vec![local.clone(), cloud.clone()],
        &["local"],
        no_rate_limit(),
        Some(rec.clone()),
    );
    let mut req = request(slack_ctx(ASK));
    req.provider = Some(ProviderId::new("cloud"));
    assert_eq!(d.read_ahead(req).await, Err(ReadAheadSkip::Cloud));
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(cloud.requests().is_empty() && local.requests().is_empty());
    assert_eq!(rec.warms(), 0);
    assert!(rec.prefixes().is_empty());
    assert!(!d.cancel_read_ahead());
}

#[tokio::test]
async fn disabled_refused_and_unavailable() {
    let local = Fake::new("local", Locality::OnDevice);
    let rec = Arc::new(Rec::default());
    let off = DraftConfig {
        read_ahead: ReadAheadConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let d = drafter(vec![local.clone()], &["local"], off, Some(rec.clone()));
    assert_eq!(
        d.read_ahead(request(slack_ctx(ASK))).await,
        Err(ReadAheadSkip::Disabled)
    );

    let d = drafter(
        vec![local.clone()],
        &["local"],
        no_rate_limit(),
        Some(rec.clone()),
    );
    let mut secure = slack_ctx(ASK);
    secure.field.secure = true;
    assert_eq!(
        d.read_ahead(request(secure)).await,
        Err(ReadAheadSkip::Refused(Refusal::SecureField))
    );
    let mut excluded = slack_ctx(ASK);
    excluded.app.id = Some("com.1password.1password".into());
    assert!(matches!(
        d.read_ahead(request(excluded)).await,
        Err(ReadAheadSkip::Refused(Refusal::ExcludedApp(_)))
    ));

    // No warmer, or no provider at all.
    let d = drafter(vec![local.clone()], &["local"], no_rate_limit(), None);
    assert_eq!(
        d.read_ahead(request(slack_ctx(ASK))).await,
        Err(ReadAheadSkip::Unavailable)
    );
    let d = drafter(vec![local.clone()], &[], no_rate_limit(), Some(rec.clone()));
    assert_eq!(
        d.read_ahead(request(slack_ctx(ASK))).await,
        Err(ReadAheadSkip::Unavailable)
    );
    assert!(rec.prefixes().is_empty() && local.requests().is_empty());
}

#[tokio::test]
async fn read_aheads_are_rate_limited() {
    let local = Fake::new("local", Locality::OnDevice);
    let rec = Arc::new(Rec::default());
    let config = DraftConfig {
        read_ahead: ReadAheadConfig {
            enabled: true,
            min_interval_ms: 60_000,
        },
        ..Default::default()
    };
    let d = drafter(vec![local], &["local"], config, Some(rec.clone()));
    d.read_ahead(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(
        d.read_ahead(request(slack_ctx("And the garden?"))).await,
        Err(ReadAheadSkip::RateLimited)
    );
    assert_eq!(rec.prefixes().len(), 1);
}

#[tokio::test]
async fn an_unchanged_prompt_is_not_sent_again() {
    let local = Fake::new("local", Locality::OnDevice);
    let rec = Arc::new(Rec::default());
    let d = drafter(
        vec![local.clone()],
        &["local"],
        no_rate_limit(),
        Some(rec.clone()),
    );
    d.read_ahead(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(
        d.read_ahead(request(slack_ctx(ASK))).await,
        Err(ReadAheadSkip::Unchanged)
    );
    // After the draft itself the slot holds that prompt (and the reply after it).
    d.begin(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(
        d.read_ahead(request(slack_ctx(ASK))).await,
        Err(ReadAheadSkip::Unchanged)
    );
    assert_eq!(rec.prefixes().len(), 1);

    // A new message changes the prompt.
    d.read_ahead(request(slack_ctx("And the garden?")))
        .await
        .unwrap();
    assert_eq!(rec.prefixes().len(), 2);
    // A draft in another thread replaced the cached prompt: the first one is sent again.
    d.begin(request(slack_ctx("Lunch?"))).await.unwrap();
    d.read_ahead(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(rec.prefixes().len(), 3);
}

#[tokio::test]
async fn cancel_read_ahead_drops_the_prefill() {
    let local = Fake::new("local", Locality::OnDevice);
    let rec = Rec::holding();
    let d = drafter(
        vec![local.clone()],
        &["local"],
        no_rate_limit(),
        Some(rec.clone()),
    );
    assert!(!d.cancel_read_ahead(), "nothing in flight");
    let task = tokio::spawn({
        let d = d.clone();
        async move { d.read_ahead(request(slack_ctx(ASK))).await }
    });
    until(|| rec.prefixes().len() == 1).await;
    assert!(d.cancel_read_ahead());
    assert_eq!(task.await.unwrap(), Err(ReadAheadSkip::Cancelled));
    assert!(!d.cancel_read_ahead());
    // The prefill may never have reached the server: the same prompt is read ahead again.
    let task = tokio::spawn({
        let d = d.clone();
        async move { d.read_ahead(request(slack_ctx(ASK))).await }
    });
    until(|| rec.prefixes().len() == 2).await;
    // A newer read-ahead replaces the one in flight.
    let newer = tokio::spawn({
        let d = d.clone();
        async move { d.read_ahead(request(slack_ctx("And the garden?"))).await }
    });
    assert_eq!(task.await.unwrap(), Err(ReadAheadSkip::Cancelled));
    until(|| rec.prefixes().len() == 3).await;
    rec.release.notify_one();
    assert!(newer.await.unwrap().is_ok());
    assert!(local.requests().is_empty());
}

#[tokio::test]
async fn trigger_prewarm_keeps_a_read_ahead() {
    let local = Fake::new("local", Locality::OnDevice);
    let rec = Arc::new(Rec::default());
    let d = drafter(
        vec![local.clone()],
        &["local"],
        no_rate_limit(),
        Some(rec.clone()),
    );
    assert_eq!(d.prewarm_idle().unwrap().as_str(), "local");
    until(|| rec.prefixes().len() == 1).await;
    d.read_ahead(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(rec.prefixes().len(), 2);

    // The trigger in the same app: connection only, the whole prompt stays cached.
    d.prewarm(&slack_ctx(ASK)).unwrap();
    until(|| rec.warms() == 1).await;
    // Even when the thread moved on: the server reuses the common prefix anyway.
    d.prewarm(&slack_ctx("And the garden?")).unwrap();
    until(|| rec.warms() == 2).await;
    assert!(d.prewarm_idle().is_none(), "the rules are cached");
    assert_eq!(rec.prefixes().len(), 2);

    // Another app was not read ahead: its trigger warms its own prefix.
    d.prewarm(&mail_ctx(ASK)).unwrap();
    until(|| rec.prefixes().len() == 3).await;
    let (_, prefix) = rec.prefixes()[2].clone();
    assert!(prefix.messages.is_empty());
    assert_eq!(prefix.cache, Some(CacheHint::keyed("draft:email")));
}

#[tokio::test]
async fn trigger_prewarm_keeps_a_read_ahead_in_flight() {
    let local = Fake::new("local", Locality::OnDevice);
    let rec = Rec::holding();
    let d = drafter(
        vec![local.clone()],
        &["local"],
        no_rate_limit(),
        Some(rec.clone()),
    );
    let task = tokio::spawn({
        let d = d.clone();
        async move { d.read_ahead(request(slack_ctx(ASK))).await }
    });
    until(|| rec.prefixes().len() == 1).await;
    d.prewarm(&slack_ctx(ASK)).unwrap();
    until(|| rec.warms() == 1).await;
    assert_eq!(rec.prefixes().len(), 1);
    rec.release.notify_one();
    assert!(task.await.unwrap().is_ok());

    // A cancelled read-ahead no longer covers the trigger.
    let task = tokio::spawn({
        let d = d.clone();
        async move { d.read_ahead(request(mail_ctx(ASK))).await }
    });
    until(|| rec.prefixes().len() == 2).await;
    assert!(d.cancel_read_ahead());
    assert_eq!(task.await.unwrap(), Err(ReadAheadSkip::Cancelled));
    d.prewarm(&mail_ctx(ASK)).unwrap();
    until(|| rec.prefixes().len() == 3).await;
    assert!(rec.prefixes()[2].1.messages.is_empty());
}

fn small_first(provider: &str) -> DraftConfig {
    DraftConfig {
        local: LocalBudget {
            provider: Some(ProviderId::new(provider)),
            ..Default::default()
        },
        ..no_rate_limit()
    }
}

#[tokio::test]
async fn local_drafts_go_to_the_small_model_first() {
    let big = Fake::new("big", Locality::OnDevice);
    let small = Fake::new("small", Locality::OnDevice);
    let rec = Arc::new(Rec::default());
    let d = drafter(
        vec![big.clone(), small.clone()],
        &["big"],
        small_first("small"),
        Some(rec.clone()),
    );
    let (started, _) = d.begin(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(started.provider.as_str(), "small");
    assert!(big.requests().is_empty());
    assert_eq!(d.prewarm_idle(), None, "the draft sent the rules to small");
    let out = d.read_ahead(request(slack_ctx("Lunch?"))).await.unwrap();
    assert_eq!(out.provider.as_str(), "small");
    assert_eq!(
        d.prewarm(&slack_ctx(ASK)).unwrap().provider.as_str(),
        "small"
    );

    // Idle warm-up warms the provider drafts will use.
    let d = drafter(
        vec![big.clone(), small.clone()],
        &["big"],
        small_first("small"),
        Some(rec.clone()),
    );
    assert_eq!(d.prewarm_idle().unwrap().as_str(), "small");

    // Listed in the tier too: tried once, first.
    let d = drafter(
        vec![big.clone(), small.clone()],
        &["big", "small"],
        small_first("small"),
        None,
    );
    let (started, _) = d.begin(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(started.provider.as_str(), "small");
    // An unknown provider is ignored.
    let d = drafter(vec![big.clone()], &["big"], small_first("nope"), None);
    let (started, _) = d.begin(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(started.provider.as_str(), "big");
}

#[tokio::test]
async fn a_failing_small_model_falls_back_to_the_tier() {
    let big = Fake::new("big", Locality::OnDevice);
    let small = Fake::failing("small", Locality::OnDevice);
    let d = drafter(
        vec![big.clone(), small.clone()],
        &["big"],
        small_first("small"),
        None,
    );
    let (started, _) = d.begin(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(started.provider.as_str(), "big");
    assert_eq!(small.requests().len(), 1);
    assert_eq!(big.requests().len(), 1);
    // While it cools down, the tier goes first.
    let (started, _) = d.begin(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(started.provider.as_str(), "big");
    assert_eq!(small.requests().len(), 1);
}

#[tokio::test]
async fn small_model_routing_leaves_cloud_and_explicit_choices_alone() {
    let cloud = Fake::new("cloud", Locality::Cloud);
    let big = Fake::new("big", Locality::OnDevice);
    let small = Fake::new("small", Locality::OnDevice);
    let all = [cloud.clone(), big.clone(), small.clone()];

    // Cloud first in the tier: unchanged.
    let d = drafter(all.to_vec(), &["cloud", "big"], small_first("small"), None);
    let (started, _) = d.begin(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(started.provider.as_str(), "cloud");

    // The request names a provider.
    let d = drafter(all.to_vec(), &["big"], small_first("small"), None);
    let mut req = request(slack_ctx(ASK));
    req.provider = Some(ProviderId::new("big"));
    let (started, _) = d.begin(req).await.unwrap();
    assert_eq!(started.provider.as_str(), "big");

    // The app names a provider.
    let mut config = small_first("small");
    config
        .app_providers
        .insert("com.tinyspeck.slackmacgap".into(), ProviderId::new("big"));
    let d = drafter(all.to_vec(), &["big"], config, None);
    let (started, _) = d.begin(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(started.provider.as_str(), "big");

    // A cloud provider named as the local one is never put first.
    let d = drafter(all.to_vec(), &["big"], small_first("cloud"), None);
    let (started, _) = d.begin(request(slack_ctx(ASK))).await.unwrap();
    assert_eq!(started.provider.as_str(), "big");
    assert!(small.requests().is_empty());
    assert_eq!(cloud.requests().len(), 1);
}

#[tokio::test]
async fn a_dropped_read_ahead_counts_as_cancelled() {
    let local = Fake::new("local", Locality::OnDevice);
    let rec = Rec::holding();
    let d = drafter(
        vec![local.clone()],
        &["local"],
        no_rate_limit(),
        Some(rec.clone()),
    );
    let task = tokio::spawn({
        let d = d.clone();
        async move { d.read_ahead(request(slack_ctx(ASK))).await }
    });
    until(|| rec.prefixes().len() == 1).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!d.cancel_read_ahead(), "nothing in flight any more");
    // The trigger warms the prefix again.
    d.prewarm(&slack_ctx(ASK)).unwrap();
    until(|| rec.prefixes().len() == 2).await;
    assert!(rec.prefixes()[1].1.messages.is_empty());
    assert_eq!(rec.warms(), 0);
}

#[tokio::test]
async fn read_ahead_matches_begin_with_an_instruction_and_several_points() {
    let local = Fake::new("local", Locality::OnDevice);
    let rec = Arc::new(Rec::default());
    let d = drafter(
        vec![local.clone()],
        &["local"],
        DraftConfig::default(),
        Some(rec.clone()),
    );
    let mut ctx = slack_ctx(
        "Hi! Can you send the Q3 numbers? How much did revenue grow? Also please let me know \
if the garden needs watering.",
    );
    ctx.thread.insert(
        0,
        ThreadMessage {
            author: None,
            text: "Morning, I'll look into it".into(),
            from_me: Some(true),
            timestamp: None,
        },
    );
    let req = || DraftRequest {
        instruction: Some("mention the growth figure".into()),
        ..request(ctx.clone())
    };
    d.read_ahead(req()).await.unwrap();
    let (_, warm) = rec.prefixes().pop().unwrap();
    d.begin(req()).await.unwrap();
    let sent = local.requests().pop().unwrap();
    assert_eq!(warm.system, sent.system);
    assert_eq!(without_breakpoints(&warm.messages), sent.messages);
    assert_eq!(warm.items, sent.items);
    let text: String = sent.messages[0]
        .content
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("<points>\n1. Can you send the Q3 numbers?\n2. How much did revenue grow?\n3. Also please let me know if the garden needs watering.\n</points>"), "{text}");
    assert!(text.contains("<instruction>mention the growth figure</instruction>"));
}
