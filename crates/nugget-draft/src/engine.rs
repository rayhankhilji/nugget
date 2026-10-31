//! The drafting engine: gate, provider choice, budgeted retrieval, voice, prompt, streaming
//! post-processing, cancellation, timings and feedback.

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::{BoxFuture, Shared};
use futures::{FutureExt, Stream, StreamExt};
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{ContextResponse, DraftContext, DraftFeedback, DraftRequest, DraftStarted};
use nugget_core::{Error, Result};
use nugget_infer::router::DEFAULT_COOLDOWN;
use nugget_infer::{
    CacheHint, ChatEvent, ChatRequest, ChatStream, Part, Provider, Router, StopReason,
};
use nugget_retrieve::{ContextOptions, PackedContext, Retriever};
use nugget_store::{AuditEntry, Store};
use nugget_voice::{Channel, VoiceHints};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::FieldKind;
use crate::classify::{DEFAULT_EXCLUDED_APPS, gate_with, normalize_app_id, url_host_path};
use crate::grounding::{Grounder, GroundingCheck, GroundingReport};
use crate::post::{Emit, PostOptions, PostProcessor, is_greeting_line, is_signoff};
use crate::prompt::{
    LengthBand, PromptInput, PromptLimits, SYSTEM_RULES, build_request, cache_key, channel_for,
    prefix_request, split_value,
};
use crate::warm::{ReadAheadConfig, ReadAheadOutcome, ReadAheadSkip};

/// Upper bound on the retrieval budget, whatever the config says.
pub const MAX_BUDGET_TOKENS: u32 = 8_000;
/// Sessions kept for feedback and timings lookups.
const MAX_SESSIONS: usize = 64;
/// Prewarmed retrievals and voice lookups kept at once.
const MAX_CACHE: usize = 16;
/// Longest final text passed to the feedback sink (characters).
const MAX_FEEDBACK_CHARS: usize = 20_000;

/// What the voice profile is asked for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VoiceQuery {
    pub channel: Channel,
    pub kind: FieldKind,
    /// Normalised app id, when known.
    pub app_id: Option<String>,
    pub recipients: Vec<String>,
    /// What the thread is about (for topical exemplars).
    pub topic: String,
    /// The draft goes to a cloud provider: exemplars should avoid sensitive messages.
    pub for_cloud: bool,
}

/// Source of voice hints (implemented by `nugget-voice`'s profile).
#[async_trait]
pub trait VoiceSource: Send + Sync {
    async fn hints(&self, query: &VoiceQuery) -> Result<VoiceHints>;
}

/// No profile: neutral hints.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoVoice;

#[async_trait]
impl VoiceSource for NoVoice {
    async fn hints(&self, query: &VoiceQuery) -> Result<VoiceHints> {
        Ok(VoiceHints {
            channel: Some(query.channel),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackOutcome {
    /// Inserted as is.
    Accepted,
    /// Inserted, then changed.
    Edited,
    Rejected,
}

impl FeedbackOutcome {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "accepted" => Ok(Self::Accepted),
            "edited" => Ok(Self::Edited),
            "rejected" => Ok(Self::Rejected),
            other => Err(Error::Invalid(format!("unknown draft outcome {other:?}"))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Edited => "edited",
            Self::Rejected => "rejected",
        }
    }
}

/// A draft's outcome with its texts, for the voice profile (which stores it encrypted).
#[derive(Debug, Clone, PartialEq)]
pub struct FeedbackRecord {
    pub stream_id: String,
    pub outcome: FeedbackOutcome,
    pub field_kind: FieldKind,
    pub channel: Channel,
    pub app_id: Option<String>,
    pub recipients: Vec<String>,
    /// What the engine emitted.
    pub draft: String,
    /// What the user finally sent, when known.
    pub final_text: Option<String>,
    pub insert_verified: Option<bool>,
    /// The draft drew on sensitive passages: the stored sample must stay out of cloud prompts.
    pub sensitive: bool,
}

/// Receives draft outcomes and final texts (implemented by `nugget-voice`).
#[async_trait]
pub trait FeedbackSink: Send + Sync {
    async fn record(&self, record: FeedbackRecord) -> Result<()>;
}

/// Opens or keeps alive a provider's connection ahead of a request (wired to `nugget-infer`'s
/// prewarm once it has one).
#[async_trait]
pub trait Warmer: Send + Sync {
    async fn warm(&self, provider: Arc<dyn Provider>);

    /// Warm the provider's prompt cache with a draft's stable prefix (see
    /// [`nugget_infer::Provider::prewarm_prefix`]): the rules and voice at the trigger, or a
    /// draft's whole prompt for a read-ahead ([`Drafter::read_ahead`], whose request ends with a
    /// [`Part::CacheBreakpoint`] so that its messages are part of the prefix). Only ever called
    /// for non-cloud providers. The default only opens the connection.
    async fn warm_prefix(&self, provider: Arc<dyn Provider>, prefix: ChatRequest) {
        let _ = prefix;
        self.warm(provider).await;
    }
}

/// Runs a draft's retrieval job (store and index calls that block between awaits). Without one
/// the job is polled wherever it is awaited; a host can move it off its async workers.
#[async_trait]
pub trait RetrievalRunner: Send + Sync {
    async fn run(&self, job: BoxFuture<'static, Result<PackedContext>>) -> Result<PackedContext>;
}

/// Drafting settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftConfig {
    /// Tier to route by when neither the request nor the app names a provider.
    pub tier: Tier,
    /// Retrieval context budget, in tokens (capped at [`MAX_BUDGET_TOKENS`]).
    pub budget_tokens: u32,
    /// The user's provider per app: keys are app ids (`com.tinyspeck.slackmacgap`, `slack.exe`)
    /// or page hosts (`mail.google.com`, matching subdomains). The named provider is used alone.
    pub app_providers: HashMap<String, ProviderId>,
    /// Send the screenshot to a vision-capable cloud provider (local providers only by default).
    pub allow_cloud_screenshots: bool,
    /// App ids and hosts where drafting is refused.
    pub excluded_apps: Vec<String>,
    /// Draft into terminal emulators ([`crate::DEFAULT_TERMINAL_APPS`]); refused by default,
    /// since inserted text could run as a command.
    pub allow_terminals: bool,
    pub temperature: Option<f32>,
    /// Retrieval that takes longer is skipped for this draft.
    pub retrieval_timeout_ms: u64,
    /// A voice lookup that takes longer falls back to neutral hints.
    pub voice_timeout_ms: u64,
    /// How long a prewarmed retrieval stays usable.
    pub prewarm_ttl_ms: u64,
    /// Grounding (AA3): the context goes in as a dated facts block (superseded facts marked,
    /// sensitive ones marked private) and the streamed draft is checked ([`Self::grounding_check`]).
    pub grounding: bool,
    /// What the streaming check does when `grounding` is on: stop a copied credential while
    /// streaming word by word (the default: it adds next to no latency), or hold and check
    /// whole sentences, values included (see `bench/README.md`, "Grounded drafts"). With
    /// grounding off, or a screenshot in the prompt, the credential stop still runs whenever a
    /// cited passage is sensitive.
    pub grounding_check: GroundingCheck,
    #[serde(skip)]
    pub limits: PromptLimits,
    /// Smaller prompts for on-device and LAN providers ([`LocalBudget`]); cloud providers keep
    /// `budget_tokens` and `limits`.
    pub local: LocalBudget,
    /// Read-ahead while a reply field keeps focus ([`Drafter::read_ahead`]).
    pub read_ahead: ReadAheadConfig,
}

/// The prompt budget for non-cloud providers, whose prompt reading dominates Double Tap latency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalBudget {
    /// Off: local providers get the cloud budget and limits.
    pub enabled: bool,
    /// Retrieval context budget, in tokens.
    pub budget_tokens: u32,
    /// Retrieved snippets kept (the best first).
    pub max_snippets: usize,
    /// Let passages marked sensitive into local prompts. Off by default: with only a few
    /// snippets a sensitive one (a card number, a password) is more likely to make the cut and
    /// a small model to paste it into a draft that is then sent from a third-party app.
    pub include_sensitive: bool,
    /// Drafts routed by tier to a non-cloud provider go to this provider first (the small
    /// Double Tap model, `local-draft`), falling back to the tier's candidates; for the router's
    /// cooldown after it fails a draft it goes after them instead. Ignored when the request or
    /// the app names a provider, the tier's first candidate is a cloud one, the router does not
    /// know it, or it is itself a cloud provider. [`Drafter::prewarm_idle`] and
    /// [`Drafter::read_ahead`] warm the same provider.
    #[serde(default)]
    pub provider: Option<ProviderId>,
    #[serde(skip, default = "PromptLimits::local")]
    pub limits: PromptLimits,
}

impl Default for LocalBudget {
    fn default() -> Self {
        Self {
            enabled: true,
            budget_tokens: 300,
            max_snippets: 3,
            include_sensitive: false,
            provider: None,
            limits: PromptLimits::local(),
        }
    }
}

impl Default for DraftConfig {
    fn default() -> Self {
        Self {
            tier: Tier::Synthesis,
            budget_tokens: 1_500,
            app_providers: HashMap::new(),
            allow_cloud_screenshots: false,
            excluded_apps: DEFAULT_EXCLUDED_APPS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            allow_terminals: false,
            temperature: None,
            retrieval_timeout_ms: 1_500,
            voice_timeout_ms: 500,
            prewarm_ttl_ms: 30_000,
            grounding: true,
            grounding_check: GroundingCheck::default(),
            limits: PromptLimits::default(),
            local: LocalBudget::default(),
            read_ahead: ReadAheadConfig::default(),
        }
    }
}

/// Where the time of one draft went. All durations are from the start of `begin`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftTimings {
    /// Retrieval (and voice) came from a `prewarm`.
    pub prewarmed: bool,
    /// Time spent waiting for retrieval in `begin` (near zero when prewarmed).
    pub retrieval_ms: Option<u64>,
    /// Time the retrieval itself took, wherever it ran.
    pub retrieval_run_ms: Option<u64>,
    pub voice_ms: Option<u64>,
    /// Until the provider produced its first event.
    pub provider_open_ms: Option<u64>,
    /// Until the first post-processed text was emitted.
    pub first_token_ms: Option<u64>,
    /// Until the stream finished (done, error or cancel).
    pub total_ms: Option<u64>,
    /// Providers tried (more than one after a fallback).
    pub attempts: u32,
    /// What the grounding check dropped (when grounding is on), set when the draft completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding: Option<Box<GroundingReport>>,
    /// Prompt tokens of the request, cached or not (from the provider's stats).
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    /// Prompt tokens served from the provider's prompt cache (llama-server `cache_n`).
    #[serde(default)]
    pub cached_prompt_tokens: Option<u64>,
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// One draft in progress or recently finished.
#[derive(Clone)]
struct Session {
    kind: FieldKind,
    app_id: Option<String>,
    recipients: Vec<String>,
    provider: ProviderId,
    cancel: CancellationToken,
    timings: Arc<Mutex<DraftTimings>>,
    draft: Arc<Mutex<String>>,
    /// A cited passage was sensitive.
    sensitive: bool,
}

#[derive(Default)]
struct Sessions {
    map: HashMap<String, Session>,
    order: VecDeque<String>,
}

impl Sessions {
    fn insert(&mut self, id: String, s: Session) {
        while self.order.len() >= MAX_SESSIONS {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        self.order.push_back(id.clone());
        self.map.insert(id, s);
    }
}

#[derive(Debug)]
struct ContextOutcome {
    ctx: Option<ContextResponse>,
    run: Duration,
}

type SharedContext = Shared<BoxFuture<'static, Arc<ContextOutcome>>>;
type SharedVoice = Shared<BoxFuture<'static, Arc<VoiceHints>>>;

#[derive(Default)]
struct Cache {
    contexts: Vec<(Instant, (String, bool), SharedContext)>,
    voices: Vec<(Instant, VoiceQuery, SharedVoice)>,
}

fn take_fresh<K: PartialEq, V: Clone>(
    list: &mut Vec<(Instant, K, V)>,
    key: &K,
    ttl: Duration,
) -> Option<V> {
    list.retain(|(at, _, _)| at.elapsed() < ttl);
    list.iter()
        .find(|(_, k, _)| k == key)
        .map(|(_, _, v)| v.clone())
}

fn put<K, V>(list: &mut Vec<(Instant, K, V)>, key: K, value: V) {
    if list.len() >= MAX_CACHE {
        list.remove(0);
    }
    list.push((Instant::now(), key, value));
}

struct Inner {
    store: Store,
    retriever: Retriever,
    router: Arc<Router>,
    voice: Arc<dyn VoiceSource>,
    feedback: RwLock<Option<Arc<dyn FeedbackSink>>>,
    warmer: RwLock<Option<Arc<dyn Warmer>>>,
    retrieval_runner: RwLock<Option<Arc<dyn RetrievalRunner>>>,
    config: DraftConfig,
    sessions: Mutex<Sessions>,
    cache: Mutex<Cache>,
    /// Per provider, the system prompt of the last draft request sent or prefix warmed. A
    /// llama-server slot holds that prompt's cache; warming the same prefix again would only cut
    /// the cache back to it (losing a repeat draft's thread), so such warms are skipped.
    last_prefix: Mutex<HashMap<ProviderId, String>>,
    /// Per provider, a fingerprint ([`fingerprint`]) of the last whole draft prompt sent, by a
    /// draft or a read-ahead; cleared when a prefix-only warm cuts the cache back to the prefix.
    /// A read-ahead of the same prompt finds it cached ([`ReadAheadSkip::Unchanged`]).
    last_full: Mutex<HashMap<ProviderId, u64>>,
    read_ahead: Mutex<ReadAheadState>,
    /// When [`LocalBudget::provider`] last failed a draft: while the router's default cooldown
    /// runs, it goes after the tier's candidates rather than before them.
    local_failed_at: Mutex<Option<Instant>>,
}

/// The read-ahead in flight and the last completed one (no prompt content, only a fingerprint).
#[derive(Default)]
struct ReadAheadState {
    /// Numbers read-aheads, so a finished one only clears its own entry.
    generation: u64,
    in_flight: Option<InFlight>,
    /// When the last read-ahead sent its prefill (the rate limit's reference).
    last_work: Option<Instant>,
    last_done: Option<ReadAheadDone>,
}

struct InFlight {
    generation: u64,
    cancel: CancellationToken,
    app: Option<String>,
    provider: ProviderId,
    /// Once the prefill is sent: its system prompt and fingerprint, undone on cancel.
    sent: Option<(String, u64)>,
}

/// Cancels its read-ahead if dropped while that one is still the one in flight.
struct InFlightGuard<'a> {
    drafter: &'a Drafter,
    generation: u64,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        let mut st = lock(&self.drafter.inner.read_ahead);
        if st
            .in_flight
            .as_ref()
            .is_some_and(|f| f.generation == self.generation)
        {
            self.drafter.cancel_in_flight(&mut st);
        }
    }
}

struct ReadAheadDone {
    app: Option<String>,
    provider: ProviderId,
    at: Instant,
}

/// The Double Tap drafting engine. Cheap to clone.
#[derive(Clone)]
pub struct Drafter {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Drafter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Drafter")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

/// What a `prewarm` started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prewarm {
    pub provider: ProviderId,
    pub locality: Locality,
    /// Retrieval was started (the hint had a subject, recipients or thread).
    pub retrieval: bool,
}

impl Drafter {
    pub fn new(
        store: Store,
        retriever: Retriever,
        router: Arc<Router>,
        voice: Arc<dyn VoiceSource>,
        config: DraftConfig,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                retriever,
                router,
                voice,
                feedback: RwLock::new(None),
                warmer: RwLock::new(None),
                retrieval_runner: RwLock::new(None),
                config,
                sessions: Mutex::new(Sessions::default()),
                cache: Mutex::new(Cache::default()),
                last_prefix: Mutex::new(HashMap::new()),
                last_full: Mutex::new(HashMap::new()),
                read_ahead: Mutex::new(ReadAheadState::default()),
                local_failed_at: Mutex::new(None),
            }),
        }
    }

    /// Where final texts from [`feedback`](Self::feedback) go.
    pub fn with_feedback_sink(self, sink: Arc<dyn FeedbackSink>) -> Self {
        self.set_feedback_sink(sink);
        self
    }

    /// Set the feedback sink on a shared drafter (e.g. once the voice profile is loaded).
    pub fn set_feedback_sink(&self, sink: Arc<dyn FeedbackSink>) {
        *self
            .inner
            .feedback
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    /// Connection prewarming used by [`prewarm`](Self::prewarm).
    pub fn with_warmer(self, warmer: Arc<dyn Warmer>) -> Self {
        *self.inner.warmer.write().unwrap_or_else(|e| e.into_inner()) = Some(warmer);
        self
    }

    /// Where retrieval runs (see [`RetrievalRunner`]); prewarmed and draft retrievals alike.
    pub fn with_retrieval_runner(self, runner: Arc<dyn RetrievalRunner>) -> Self {
        *self
            .inner
            .retrieval_runner
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(runner);
        self
    }

    pub fn config(&self) -> &DraftConfig {
        &self.inner.config
    }

    /// Start work as soon as the trigger fires, before the full context is read: resolve the
    /// provider, start retrieval and the voice lookup from whatever the hint already has, and
    /// with a [`Warmer`] get the provider ready. For a cloud provider that only opens the
    /// connection; for a non-cloud one, once the voice lookup is in (bounded by
    /// `voice_timeout_ms`), it warms the prompt cache with the exact system prefix `begin` will
    /// send ([`Warmer::warm_prefix`]), unless that is the system prompt of the last request sent
    /// or warmed on that provider (already cached; warming it again would cut a llama-server
    /// slot's cache back to the prefix), when it only opens the connection. It also only opens
    /// the connection when a read-ahead ([`read_ahead`](Self::read_ahead)) for the same app and
    /// provider is in flight or completed within `prewarm_ttl_ms`: that read-ahead holds (or is
    /// filling) the slot with the whole prompt, which a prefix-only warm would cut back to the
    /// prefix; `begin` then reads only what changed since. `begin` reuses the retrieval and voice
    /// results when its context gives the same retrieval query. Returns `None` when drafting
    /// would be refused.
    pub fn prewarm(&self, hint: &DraftContext) -> Option<Prewarm> {
        let kind = gate_with(
            hint,
            &self.inner.config.excluded_apps,
            self.inner.config.allow_terminals,
        )
        .ok()?;
        let (candidates, _) = self.candidates(hint, None).ok()?;
        let provider = candidates.into_iter().next()?;
        let cloud = provider.meta().locality == Locality::Cloud;
        let warmer = self.warmer();
        let query = retrieval_query(hint);
        let retrieval = !query.is_empty();
        if retrieval {
            let (fut, _) = self.context_future(&query, cloud);
            tokio::spawn(fut);
        }
        let voice = self.voice_future(voice_query(hint, kind, &query, cloud));
        tokio::spawn(voice.clone());
        if let Some(w) = warmer {
            let p = provider.clone();
            let app = hint.app.id.as_deref().map(normalize_app_id);
            if cloud {
                // Connection only: no prompt content leaves before the draft itself.
                tokio::spawn(async move { w.warm(p).await });
            } else if self.read_ahead_covers(app.as_deref(), p.id()) {
                // The read-ahead holds the whole prompt: keep it, only open the connection.
                tracing::debug!(provider = %p.id(), "draft prewarm: read-ahead covers the prefix");
                tokio::spawn(async move { w.warm(p).await });
            } else {
                // The same system prefix `begin` will send for this field, once the voice is in.
                // Skipped (connection only) when it is the provider's last prefix: it is cached.
                let limits = self.limits_for(false);
                let timeout = Duration::from_millis(self.inner.config.voice_timeout_ms);
                let inner = self.inner.clone();
                tokio::spawn(async move {
                    let prefix = match tokio::time::timeout(timeout, voice).await {
                        Ok(v) => {
                            let band = LengthBand::for_draft(kind, &v);
                            Some(prefix_request(kind, &v, &band, &limits))
                        }
                        Err(_) => None,
                    };
                    match prefix {
                        Some(prefix) if note_prefix(&inner, p.id(), prefix.system.as_deref()) => {
                            forget_full(&inner, p.id());
                            w.warm_prefix(p, prefix).await;
                        }
                        _ => w.warm(p).await,
                    }
                });
            }
        }
        Some(Prewarm {
            provider: provider.id().clone(),
            locality: provider.meta().locality,
            retrieval,
        })
    }

    /// Warm the prompt cache of the provider drafts route to by default with the fixed drafting
    /// rules, ahead of any trigger (when Double Tap is enabled or the local server is ready): the
    /// first provider a draft naming no provider would try, [`LocalBudget::provider`] included.
    /// Only non-cloud providers are warmed this way: the prefix is the rules and a neutral chat
    /// voice block, so at least the rules are cached. Returns the provider warmed; `None` when
    /// there is no warmer, no tokio runtime, the default provider is a cloud one, or a draft,
    /// warm or read-ahead on it already sent the rules.
    pub fn prewarm_idle(&self) -> Option<ProviderId> {
        let warmer = self.warmer()?;
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let (candidates, _) = self.candidates(&DraftContext::default(), None).ok()?;
        let provider = candidates.into_iter().next()?;
        if provider.meta().locality == Locality::Cloud {
            return None;
        }
        // Any earlier draft or warm on this provider already cached the rules.
        let rules_cached = self
            .inner
            .last_prefix
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider.id())
            .is_some_and(|s| s.starts_with(SYSTEM_RULES));
        if rules_cached {
            return None;
        }
        let prefix = idle_prefix(&self.limits_for(false));
        note_prefix(&self.inner, provider.id(), prefix.system.as_deref());
        forget_full(&self.inner, provider.id());
        let id = provider.id().clone();
        handle.spawn(async move { warmer.warm_prefix(provider, prefix).await });
        Some(id)
    }

    fn warmer(&self) -> Option<Arc<dyn Warmer>> {
        self.inner
            .warmer
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Prompt caps for a provider locality ([`LocalBudget`] for non-cloud ones when enabled).
    fn limits_for(&self, cloud: bool) -> PromptLimits {
        let c = &self.inner.config;
        if !cloud && c.local.enabled {
            c.local.limits
        } else {
            c.limits
        }
    }

    /// Retrieval budget (tokens) and snippet cap for a provider locality.
    fn budget_for(&self, cloud: bool) -> (usize, Option<usize>) {
        let c = &self.inner.config;
        if !cloud && c.local.enabled {
            (
                c.local.budget_tokens.min(MAX_BUDGET_TOKENS) as usize,
                Some(c.local.max_snippets),
            )
        } else {
            (c.budget_tokens.clamp(1, MAX_BUDGET_TOKENS) as usize, None)
        }
    }

    /// Start a draft. Refuses secure fields, excluded apps and search boxes; picks the provider
    /// first (the request's, else the user's choice for this app, else the configured tier),
    /// packs the retrieval context for that provider's locality (sensitive items withheld from
    /// cloud providers), builds the prompt and opens the stream. If routing by tier, a provider
    /// that fails before its first event is skipped for the next one.
    pub async fn begin(&self, req: DraftRequest) -> Result<(DraftStarted, DraftStream)> {
        let t0 = Instant::now();
        let ctx = &req.context;
        let config = &self.inner.config;
        let kind = gate_with(ctx, &config.excluded_apps, config.allow_terminals)?;
        let (candidates, explicit) = self.candidates(ctx, req.provider.as_ref())?;
        let query = retrieval_query(ctx);
        let mut timings = DraftTimings::default();
        let mut last_err = None;

        for provider in candidates {
            let meta = provider.meta();
            let cloud = meta.locality == Locality::Cloud;
            timings.attempts += 1;

            let (context, voice) = self.prepare(ctx, kind, &query, cloud, &mut timings).await;
            // Belt and braces: nothing marked sensitive is ever sent to a cloud model.
            if cloud && context.citations.iter().any(|c| c.sensitive) {
                return Err(Error::Integrity(
                    "refusing to send sensitive passages to a cloud provider".into(),
                ));
            }
            let (chat, screenshot, band) =
                self.draft_request(&req, kind, provider.id(), cloud, &context, &voice);

            note_prefix(&self.inner, provider.id(), chat.system.as_deref());
            let full = (!cloud).then(|| fingerprint(&chat));
            match open_stream(provider.as_ref(), chat).await {
                Ok(stream) => {
                    self.inner.router.report_success(provider.id());
                    self.note_local_outcome(provider.id(), true);
                    if let Some(fp) = full {
                        // The slot now holds this prompt (and the reply after it).
                        lock(&self.inner.last_full).insert(provider.id().clone(), fp);
                    }
                    timings.provider_open_ms = Some(ms(t0.elapsed()));
                    let ground = stream_check(
                        config,
                        screenshot,
                        ctx,
                        &context,
                        req.instruction.as_deref(),
                        &voice,
                    );
                    return Ok(self.start(
                        provider, kind, ctx, context, screenshot, band, stream, t0, timings, ground,
                    ));
                }
                Err(e) if !explicit && is_fallback_error(&e) => {
                    tracing::warn!(provider = %provider.id(), error = %e, "draft: provider failed; trying next");
                    self.inner.router.report_failure(provider.id());
                    self.note_local_outcome(provider.id(), false);
                    last_err = Some(e);
                }
                Err(e) => {
                    if is_fallback_error(&e) {
                        self.inner.router.report_failure(provider.id());
                    }
                    return Err(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| Error::NotFound("no provider could draft".into())))
    }

    /// The provider request a draft sends: the one code path for [`begin`](Self::begin) and
    /// [`read_ahead`](Self::read_ahead), so that a read-ahead prefills exactly what the draft
    /// will send (system, messages, cache hint `draft:<channel>` for non-cloud providers).
    /// Returns the request, whether the screenshot is attached, and the length band.
    fn draft_request(
        &self,
        req: &DraftRequest,
        kind: FieldKind,
        provider: &ProviderId,
        cloud: bool,
        context: &ContextResponse,
        voice: &VoiceHints,
    ) -> (ChatRequest, bool, LengthBand) {
        let config = &self.inner.config;
        let ctx = &req.context;
        let screenshot = ctx.thread.is_empty()
            && ctx
                .screenshot_png_base64
                .as_deref()
                .is_some_and(|s| !s.is_empty())
            && self
                .inner
                .router
                .capabilities(provider)
                .is_some_and(|c| c.vision)
            && (!cloud || config.allow_cloud_screenshots);
        let band = LengthBand::for_draft(kind, voice);
        let input = PromptInput {
            kind,
            ctx,
            voice,
            context,
            instruction: req.instruction.as_deref(),
            screenshot,
            band,
            limits: self.limits_for(cloud),
            grounding: config.grounding,
        };
        let max_tokens = req
            .max_tokens
            .unwrap_or_else(|| band.max_tokens())
            .clamp(16, 4_096);
        let mut chat = build_request(&input, max_tokens, config.temperature);
        if !cloud {
            // Routes llama-server requests sharing this prefix (with `slot_affinity`); cloud
            // requests stay exactly as they were.
            chat = chat.with_cache(CacheHint::keyed(cache_key(kind)));
        }
        (chat, screenshot, band)
    }

    /// Read ahead while a reply field keeps focus: prefill the whole prompt
    /// [`begin`](Self::begin) would send for `req` into a non-cloud provider's prompt cache
    /// (llama-server `n_predict: 0`), generating nothing, so that at the trigger only what
    /// changed since is read. The shell calls it debounced and cancels it on blur
    /// ([`cancel_read_ahead`](Self::cancel_read_ahead)).
    ///
    /// In order, it skips when: `[draft.read_ahead]` is off ([`ReadAheadSkip::Disabled`]);
    /// drafting would be refused here ([`ReadAheadSkip::Refused`]); the draft would go to a
    /// cloud provider ([`ReadAheadSkip::Cloud`]: nothing at all is sent, not even a connection
    /// prewarm, and no retrieval runs); there is no [`Warmer`] or no available provider
    /// ([`ReadAheadSkip::Unavailable`]); a read-ahead sent its prefill less than
    /// `min_interval_ms` ago ([`ReadAheadSkip::RateLimited`]). Otherwise it cancels any
    /// read-ahead in flight, prepares the draft exactly as `begin` does (retrieval and voice
    /// through the same caches a trigger reuses, the same budget and limits) and builds the
    /// same request. When that prompt is the last one sent to the provider (by a draft or a
    /// read-ahead) and the provider's cache still holds it, nothing is sent
    /// ([`ReadAheadSkip::Unchanged`]). Else the request, with a trailing
    /// [`Part::CacheBreakpoint`] so that its messages count as the prefix (the OpenAI-compatible
    /// adapter drops the marker, so the server sees `begin`'s messages), goes to
    /// [`Warmer::warm_prefix`]. Cancelled at any point it returns [`ReadAheadSkip::Cancelled`]
    /// and the prefill request is dropped.
    ///
    /// Nothing is audited or stored: the engine keeps only a fingerprint of the prompt, the
    /// app id and timestamps in memory. The provider records the egress (feature `draft`, the
    /// cited items) like any draft request.
    pub async fn read_ahead(
        &self,
        req: DraftRequest,
    ) -> std::result::Result<ReadAheadOutcome, ReadAheadSkip> {
        let config = &self.inner.config;
        if !config.read_ahead.enabled {
            return Err(ReadAheadSkip::Disabled);
        }
        let ctx = &req.context;
        let kind = gate_with(ctx, &config.excluded_apps, config.allow_terminals)
            .map_err(ReadAheadSkip::Refused)?;
        let (candidates, _) = self
            .candidates(ctx, req.provider.as_ref())
            .map_err(|_| ReadAheadSkip::Unavailable)?;
        let provider = candidates
            .into_iter()
            .next()
            .ok_or(ReadAheadSkip::Unavailable)?;
        if provider.meta().locality == Locality::Cloud {
            return Err(ReadAheadSkip::Cloud);
        }
        if !provider.available() {
            return Err(ReadAheadSkip::Unavailable);
        }
        let warmer = self.warmer().ok_or(ReadAheadSkip::Unavailable)?;
        let app = ctx.app.id.as_deref().map(normalize_app_id);
        let min_interval = Duration::from_millis(config.read_ahead.min_interval_ms);

        let (generation, cancel) = {
            let mut st = lock(&self.inner.read_ahead);
            if st.last_work.is_some_and(|t| t.elapsed() < min_interval) {
                return Err(ReadAheadSkip::RateLimited);
            }
            self.cancel_in_flight(&mut st);
            st.generation += 1;
            let cancel = CancellationToken::new();
            st.in_flight = Some(InFlight {
                generation: st.generation,
                cancel: cancel.clone(),
                app: app.clone(),
                provider: provider.id().clone(),
                sent: None,
            });
            (st.generation, cancel)
        };
        // Dropped before it finishes (the caller went away): cancelled like a blur.
        let guard = InFlightGuard {
            drafter: self,
            generation,
        };
        let outcome = self
            .read_ahead_in(&req, kind, provider, warmer, generation, &cancel)
            .await;
        std::mem::forget(guard);
        let mut st = lock(&self.inner.read_ahead);
        let finished = st.in_flight.take_if(|f| f.generation == generation);
        if let Some(f) = finished
            && matches!(outcome, Ok(_) | Err(ReadAheadSkip::Unchanged))
        {
            st.last_done = Some(ReadAheadDone {
                app: f.app,
                provider: f.provider,
                at: Instant::now(),
            });
        }
        outcome
    }

    /// The part of [`read_ahead`](Self::read_ahead) after its checks: prepare, compare, send.
    async fn read_ahead_in(
        &self,
        req: &DraftRequest,
        kind: FieldKind,
        provider: Arc<dyn Provider>,
        warmer: Arc<dyn Warmer>,
        generation: u64,
        cancel: &CancellationToken,
    ) -> std::result::Result<ReadAheadOutcome, ReadAheadSkip> {
        let t0 = Instant::now();
        let query = retrieval_query(&req.context);
        let mut timings = DraftTimings::default();
        let (context, voice) = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ReadAheadSkip::Cancelled),
            r = self.prepare(&req.context, kind, &query, false, &mut timings) => r,
        };
        let (chat, _, _) = self.draft_request(req, kind, provider.id(), false, &context, &voice);
        let prepare_ms = ms(t0.elapsed());
        let fp = fingerprint(&chat);
        let system = chat.system.clone().unwrap_or_default();
        let id = provider.id().clone();
        let prompt_chars = prompt_chars(&chat);
        {
            let mut st = lock(&self.inner.read_ahead);
            // Registered and checked under one lock, so a cancel either came first or undoes
            // exactly what this records.
            let Some(f) = st
                .in_flight
                .as_mut()
                .filter(|f| f.generation == generation && !f.cancel.is_cancelled())
            else {
                return Err(ReadAheadSkip::Cancelled);
            };
            let cached = lock(&self.inner.last_full).get(&id) == Some(&fp)
                && lock(&self.inner.last_prefix).get(&id) == Some(&system);
            if cached {
                tracing::debug!(provider = %id, "draft read-ahead: prompt already cached");
                return Err(ReadAheadSkip::Unchanged);
            }
            f.sent = Some((system.clone(), fp));
            st.last_work = Some(Instant::now());
            note_prefix(&self.inner, &id, Some(&system));
            lock(&self.inner.last_full).insert(id.clone(), fp);
        }
        let warm_req = with_trailing_breakpoint(chat);
        let t1 = Instant::now();
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                tracing::debug!(provider = %id, "draft read-ahead cancelled");
                return Err(ReadAheadSkip::Cancelled);
            }
            () = warmer.warm_prefix(provider, warm_req) => {}
        }
        let warm_ms = ms(t1.elapsed());
        tracing::debug!(provider = %id, prompt_chars, prepare_ms, warm_ms, "draft read-ahead done");
        Ok(ReadAheadOutcome {
            provider: id,
            prompt_chars,
            prepare_ms,
            warm_ms,
        })
    }

    /// Cancel the read-ahead in progress, if any (focus left the field). Returns whether one was
    /// running. Its prefill request is dropped, and the provider is no longer assumed to hold
    /// its prompt.
    pub fn cancel_read_ahead(&self) -> bool {
        let mut st = lock(&self.inner.read_ahead);
        self.cancel_in_flight(&mut st)
    }

    /// Cancel and forget the read-ahead in flight; undo what it recorded as cached.
    fn cancel_in_flight(&self, st: &mut ReadAheadState) -> bool {
        let Some(f) = st.in_flight.take() else {
            return false;
        };
        let running = !f.cancel.is_cancelled();
        f.cancel.cancel();
        if let Some((system, fp)) = f.sent {
            let mut full = lock(&self.inner.last_full);
            if full.get(&f.provider) == Some(&fp) {
                full.remove(&f.provider);
            }
            // The prefill may not have reached the server: let the trigger warm the prefix.
            let mut prefix = lock(&self.inner.last_prefix);
            if prefix.get(&f.provider) == Some(&system) {
                prefix.remove(&f.provider);
            }
        }
        running
    }

    /// Whether a read-ahead for this app and provider is in flight, or completed within
    /// `prewarm_ttl_ms`.
    fn read_ahead_covers(&self, app: Option<&str>, provider: &ProviderId) -> bool {
        let ttl = Duration::from_millis(self.inner.config.prewarm_ttl_ms);
        let st = lock(&self.inner.read_ahead);
        let in_flight = st.in_flight.as_ref().is_some_and(|f| {
            !f.cancel.is_cancelled() && f.app.as_deref() == app && &f.provider == provider
        });
        let done = st.last_done.as_ref().is_some_and(|d| {
            d.at.elapsed() < ttl && d.app.as_deref() == app && &d.provider == provider
        });
        in_flight || done
    }

    /// Track [`LocalBudget::provider`]'s health for [`candidates`](Self::candidates).
    fn note_local_outcome(&self, provider: &ProviderId, ok: bool) {
        if self.inner.config.local.provider.as_ref() != Some(provider) {
            return;
        }
        *lock(&self.inner.local_failed_at) = if ok { None } else { Some(Instant::now()) };
    }

    /// Cancel a draft in progress. Returns whether it was known and still running.
    pub fn cancel(&self, stream_id: &str) -> bool {
        let sessions = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match sessions.map.get(stream_id) {
            Some(s) if !s.cancel.is_cancelled() => {
                s.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// Timings of a recent draft.
    pub fn timings(&self, stream_id: &str) -> Option<DraftTimings> {
        let sessions = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        sessions
            .map
            .get(stream_id)
            .map(|s| s.timings.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    /// Record what happened to a draft: an audit event (no text) in the encrypted store, and
    /// the texts to the [`FeedbackSink`] for the voice profile.
    pub async fn feedback(&self, fb: DraftFeedback) -> Result<()> {
        let outcome = FeedbackOutcome::parse(&fb.outcome)?;
        let session = {
            let sessions = self
                .inner
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            sessions.map.get(&fb.stream_id).cloned()
        };
        let draft = session
            .as_ref()
            .map(|s| s.draft.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .unwrap_or_default();
        let final_text = fb
            .final_text
            .as_deref()
            .map(|t| t.chars().take(MAX_FEEDBACK_CHARS).collect::<String>());
        let detail = json!({
            "outcome": outcome.as_str(),
            "known": session.is_some(),
            "field_kind": session.as_ref().map(|s| s.kind.as_str()),
            "provider": session.as_ref().map(|s| s.provider.as_str()),
            "insert_verified": fb.insert_verified,
            "draft_words": crate::post::count_words(&draft),
            "final_words": final_text.as_deref().map(crate::post::count_words),
            "final_matches_draft": final_text.as_deref().map(|t| t.trim() == draft.trim()),
        });
        let store = self.inner.store.clone();
        let entry = AuditEntry {
            actor: "user".into(),
            action: "draft.feedback".into(),
            target: fb.stream_id.clone(),
            detail,
        };
        tokio::task::spawn_blocking(move || store.append_audit(&entry))
            .await
            .map_err(|e| Error::Other(format!("audit task failed: {e}")))??;

        let sink = self
            .inner
            .feedback
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let (Some(sink), Some(s)) = (sink, session) {
            sink.record(FeedbackRecord {
                stream_id: fb.stream_id,
                outcome,
                field_kind: s.kind,
                channel: channel_for(s.kind),
                app_id: s.app_id,
                recipients: s.recipients,
                draft,
                final_text,
                insert_verified: fb.insert_verified,
                sensitive: s.sensitive,
            })
            .await?;
        }
        Ok(())
    }

    /// Providers to try, and whether the choice was explicit (request or per-app setting).
    /// Routed by tier, providers that are not [`available`](Provider::available) are left out,
    /// and a non-cloud first candidate (before that) puts [`LocalBudget::provider`] (when set,
    /// known to the router and not a cloud provider) first, the tier's candidates after it as
    /// fallback; while it is cooling down after a failure it goes after them instead.
    fn candidates(
        &self,
        ctx: &DraftContext,
        requested: Option<&ProviderId>,
    ) -> Result<(Vec<Arc<dyn Provider>>, bool)> {
        let router = &self.inner.router;
        let chosen = requested.cloned().or_else(|| self.app_choice(ctx));
        if let Some(id) = chosen {
            let p = router
                .provider(&id)
                .ok_or_else(|| Error::NotFound(format!("provider {id} is not configured")))?;
            return Ok((vec![p], true));
        }
        let mut c = router.candidates(self.inner.config.tier, &Capabilities::default());
        let no_provider = || {
            Error::NotFound(format!(
                "no provider is available for the {:?} tier",
                self.inner.config.tier
            ))
        };
        let routed_local = c.first().ok_or_else(no_provider)?.meta().locality != Locality::Cloud;
        // Unavailable providers (a runtime slot without its model) are skipped, as `begin`'s
        // fallback would skip them, so prewarm and read-ahead report the provider drafts use.
        c.retain(|p| p.available());
        let local = self
            .inner
            .config
            .local
            .provider
            .as_ref()
            .and_then(|id| router.provider(id))
            .filter(|p| p.available() && p.meta().locality != Locality::Cloud);
        if let Some(local) = local
            && routed_local
        {
            c.retain(|p| p.id() != local.id());
            let cooling =
                lock(&self.inner.local_failed_at).is_some_and(|t| t.elapsed() < DEFAULT_COOLDOWN);
            if cooling {
                c.push(local);
            } else {
                c.insert(0, local);
            }
        }
        if c.is_empty() {
            return Err(no_provider());
        }
        Ok((c, false))
    }

    /// The user's provider for this app: by app id, then by page host (most specific first).
    fn app_choice(&self, ctx: &DraftContext) -> Option<ProviderId> {
        let map = &self.inner.config.app_providers;
        if map.is_empty() {
            return None;
        }
        let normalized: HashMap<String, &ProviderId> = map
            .iter()
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v))
            .collect();
        if let Some(id) = ctx.app.id.as_deref() {
            let raw = id.trim().to_ascii_lowercase();
            let norm = normalize_app_id(id);
            if let Some(p) = normalized.get(&raw).copied().or_else(|| {
                map.iter()
                    .find(|(k, _)| normalize_app_id(k) == norm)
                    .map(|(_, v)| v)
            }) {
                return Some(p.clone());
            }
        }
        let (host, _) = ctx
            .app
            .url
            .as_deref()
            .map(url_host_path)
            .unwrap_or_default();
        let mut h = host.as_str();
        while !h.is_empty() {
            if let Some(p) = normalized.get(h) {
                return Some((*p).clone());
            }
            h = h.split_once('.').map_or("", |(_, rest)| rest);
        }
        None
    }

    /// Retrieval for `query` packed for this locality, shared with any prewarm of the same query.
    /// Also returns whether it was already running or done (prewarmed).
    fn context_future(&self, query: &str, cloud: bool) -> (SharedContext, bool) {
        let ttl = Duration::from_millis(self.inner.config.prewarm_ttl_ms);
        let key = (query.to_owned(), cloud);
        let mut cache = self.inner.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = take_fresh(&mut cache.contexts, &key, ttl) {
            return (f, true);
        }
        let retriever = self.inner.retriever.clone();
        let runner = self
            .inner
            .retrieval_runner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // The cache key's `cloud` decides the budget too, so each locality gets its own packing.
        let (budget, max_snippets) = self.budget_for(cloud);
        let local = &self.inner.config.local;
        let exclude_sensitive = cloud || (local.enabled && !local.include_sensitive);
        let q = query.to_owned();
        let fut = async move {
            let started = Instant::now();
            let opts = ContextOptions { exclude_sensitive };
            let job = async move { retriever.pack_context_with(&q, budget, opts).await }.boxed();
            let packed = match runner {
                Some(r) => r.run(job).await,
                None => job.await,
            };
            let ctx = match packed {
                Ok(p) => {
                    let mut r = p.response;
                    if let Some(max) = max_snippets {
                        keep_best_snippets(&mut r, max);
                    }
                    Some(r)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "draft: retrieval failed; drafting without context");
                    None
                }
            };
            Arc::new(ContextOutcome {
                ctx,
                run: started.elapsed(),
            })
        }
        .boxed()
        .shared();
        put(&mut cache.contexts, key, fut.clone());
        (fut, false)
    }

    fn voice_future(&self, query: VoiceQuery) -> SharedVoice {
        let ttl = Duration::from_millis(self.inner.config.prewarm_ttl_ms);
        let mut cache = self.inner.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = take_fresh(&mut cache.voices, &query, ttl) {
            return f;
        }
        let voice = self.inner.voice.clone();
        let q = query.clone();
        let fut = async move {
            match voice.hints(&q).await {
                Ok(h) => Arc::new(h),
                Err(e) => {
                    tracing::warn!(error = %e, "draft: voice profile unavailable");
                    Arc::new(VoiceHints {
                        channel: Some(q.channel),
                        ..Default::default()
                    })
                }
            }
        }
        .boxed()
        .shared();
        put(&mut cache.voices, query, fut.clone());
        fut
    }

    /// Retrieval context and voice hints for one provider locality, concurrently, each bounded
    /// by its timeout. A slow or failed retrieval drafts without context rather than failing.
    async fn prepare(
        &self,
        ctx: &DraftContext,
        kind: FieldKind,
        query: &str,
        cloud: bool,
        timings: &mut DraftTimings,
    ) -> (ContextResponse, VoiceHints) {
        let config = &self.inner.config;
        let empty = ContextResponse {
            text: String::new(),
            citations: Vec::new(),
            excluded_sensitive: 0,
        };
        let vq = voice_query(ctx, kind, query, cloud);
        let (ctx_fut, prewarmed_ctx) = if query.is_empty() {
            (None, false)
        } else {
            let (f, cached) = self.context_future(query, cloud);
            // Drive it even if this call stops waiting, so a later draft can reuse it.
            tokio::spawn(f.clone());
            (Some(f), cached)
        };
        let voice_fut = self.voice_future(vq);
        tokio::spawn(voice_fut.clone());

        let retrieval = async {
            let started = Instant::now();
            let out = match ctx_fut {
                None => None,
                Some(f) => {
                    match tokio::time::timeout(
                        Duration::from_millis(config.retrieval_timeout_ms),
                        f,
                    )
                    .await
                    {
                        Ok(o) => Some(o),
                        Err(_) => {
                            tracing::warn!("draft: retrieval timed out; drafting without context");
                            None
                        }
                    }
                }
            };
            (out, started.elapsed())
        };
        let voice = async {
            let started = Instant::now();
            let v = tokio::time::timeout(Duration::from_millis(config.voice_timeout_ms), voice_fut)
                .await
                .ok();
            (v, started.elapsed())
        };
        let ((outcome, waited), (voice, voice_took)) = tokio::join!(retrieval, voice);
        timings.prewarmed = prewarmed_ctx;
        timings.retrieval_ms = Some(ms(waited));
        timings.retrieval_run_ms = outcome.as_ref().map(|o| ms(o.run));
        timings.voice_ms = Some(ms(voice_took));
        let context = outcome.and_then(|o| o.ctx.clone()).unwrap_or(empty);
        let voice = voice.map(|v| (*v).clone()).unwrap_or_else(|| VoiceHints {
            channel: Some(channel_for(kind)),
            ..Default::default()
        });
        (context, voice)
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &self,
        provider: Arc<dyn Provider>,
        kind: FieldKind,
        ctx: &DraftContext,
        context: ContextResponse,
        screenshot: bool,
        band: LengthBand,
        stream: ChatStream,
        t0: Instant,
        timings: DraftTimings,
        ground: Option<Grounder>,
    ) -> (DraftStarted, DraftStream) {
        let stream_id = uuid::Uuid::new_v4().to_string();
        let meta = provider.meta();
        let cancel = CancellationToken::new();
        let timings = Arc::new(Mutex::new(timings));
        let draft = Arc::new(Mutex::new(String::new()));
        let app_id = ctx.app.id.as_deref().map(normalize_app_id);
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                stream_id.clone(),
                Session {
                    kind,
                    app_id: app_id.clone(),
                    recipients: ctx.recipients.clone(),
                    provider: provider.id().clone(),
                    cancel: cancel.clone(),
                    timings: timings.clone(),
                    draft: draft.clone(),
                    sensitive: context.citations.iter().any(|c| c.sensitive),
                },
            );
        tracing::info!(
            stream = %stream_id,
            provider = %provider.id(),
            kind = kind.as_str(),
            passages = context.citations.len(),
            excluded_sensitive = context.excluded_sensitive,
            screenshot,
            "draft started"
        );
        let store = self.inner.store.clone();
        let entry = AuditEntry {
            actor: "daemon".into(),
            action: "draft.begin".into(),
            target: provider.id().as_str().to_owned(),
            detail: json!({
                "stream_id": stream_id,
                "field_kind": kind.as_str(),
                "app": app_id,
                "model": meta.model,
                "locality": meta.locality,
                "passages": context.citations.len(),
                "excluded_sensitive": context.excluded_sensitive,
                "screenshot": screenshot,
            }),
        };
        tokio::task::spawn_blocking(move || {
            if let Err(e) = store.append_audit(&entry) {
                tracing::warn!(error = %e, "draft: audit append failed");
            }
        });

        let opts = post_options(kind, ctx, band);
        let started = DraftStarted {
            stream_id: stream_id.clone(),
            provider: provider.id().clone(),
            locality: meta.locality,
            field_kind: kind.as_str().to_owned(),
            citations: context.citations,
            excluded_sensitive: context.excluded_sensitive,
        };
        let state = PumpState {
            upstream: Some(stream),
            post: PostProcessor::new(opts),
            ground,
            pending: VecDeque::new(),
            cancel: cancel.clone(),
            timings: timings.clone(),
            draft,
            t0,
            done: false,
        };
        let inner = futures::stream::unfold(state, |mut st| async move {
            let item = st.next_item().await?;
            Some((item, st))
        })
        .boxed();
        (
            started,
            DraftStream {
                inner,
                stream_id,
                cancel,
                timings,
            },
        )
    }
}

/// The streaming check on a draft: the configured grounding check; with grounding off, or with
/// a screenshot (values read off it can't be checked), still the credential stop
/// ([`GroundingCheck::Secrets`]) whenever a cited passage is sensitive, so that a credential in
/// the prompt never goes into a message whatever the settings or the screen ask.
fn stream_check(
    config: &DraftConfig,
    screenshot: bool,
    ctx: &DraftContext,
    context: &ContextResponse,
    instruction: Option<&str>,
    voice: &VoiceHints,
) -> Option<Grounder> {
    let check = if config.grounding && !screenshot {
        config.grounding_check
    } else if context.citations.iter().any(|c| c.sensitive) {
        GroundingCheck::Secrets
    } else {
        return None;
    };
    Some(Grounder::for_draft(ctx, context, instruction, voice).with_check(check))
}

/// Post-processing settings for this field.
pub fn post_options(kind: FieldKind, ctx: &DraftContext, band: LengthBand) -> PostOptions {
    let (before, _, after) = split_value(&ctx.field.value, ctx.field.selection);
    let strip_greeting = before
        .lines()
        .find(|l| !l.trim().is_empty())
        .is_some_and(is_greeting_line);
    // Sign-offs below the caret, above any quoted earlier message.
    let strip_signoff = after
        .lines()
        .take_while(|l| {
            let t = l.trim_start();
            !(t.starts_with('>')
                || t.starts_with("-----")
                || (t.starts_with("On ") && t.trim_end().ends_with("wrote:")))
        })
        .any(is_signoff);
    let role = ctx.field.role.to_ascii_lowercase();
    PostOptions {
        plain_text: matches!(
            kind,
            FieldKind::ChatMessage | FieldKind::EmailBody | FieldKind::FormField
        ),
        single_line: kind == FieldKind::FormField || role == "text_field" || role == "combo_box",
        strip_greeting,
        strip_signoff,
        max_words: Some(band.hard_max),
    }
}

/// The retrieval query: subject, recipients and the newest thread messages, capped.
pub fn retrieval_query(ctx: &DraftContext) -> String {
    const PER_MESSAGE: usize = 300;
    const TOTAL: usize = 800;
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = ctx
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(s.to_owned());
    }
    let mut msgs: Vec<&str> = ctx
        .thread
        .iter()
        .rev()
        .filter(|m| m.from_me != Some(true))
        .map(|m| m.text.trim())
        .filter(|t| !t.is_empty())
        .take(3)
        .collect();
    msgs.reverse();
    for m in msgs {
        parts.push(m.chars().take(PER_MESSAGE).collect());
    }
    for r in ctx.recipients.iter().take(5) {
        // "Sam Lee <sam@example.com>" → "Sam Lee".
        let name = r.split('<').next().unwrap_or(r).trim();
        let name = if name.is_empty() { r.as_str() } else { name };
        parts.push(name.split('@').next().unwrap_or(name).to_owned());
    }
    let q = parts.join("\n");
    q.chars().take(TOTAL).collect::<String>().trim().to_owned()
}

fn voice_query(ctx: &DraftContext, kind: FieldKind, topic: &str, for_cloud: bool) -> VoiceQuery {
    VoiceQuery {
        channel: channel_for(kind),
        kind,
        app_id: ctx.app.id.as_deref().map(normalize_app_id),
        recipients: ctx.recipients.clone(),
        topic: topic.to_owned(),
        for_cloud,
    }
}

/// Lock a mutex, recovering from poisoning.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A fingerprint of a request's prompt (system and messages); no content is kept.
fn fingerprint(req: &ChatRequest) -> u64 {
    let mut h = DefaultHasher::new();
    req.system.hash(&mut h);
    serde_json::to_string(&req.messages)
        .unwrap_or_default()
        .hash(&mut h);
    h.finish()
}

/// Characters of a request's prompt text (system and text parts).
fn prompt_chars(req: &ChatRequest) -> usize {
    let system = req.system.as_deref().map_or(0, |s| s.chars().count());
    let messages: usize = req
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .map(|p| match p {
            Part::Text { text } => text.chars().count(),
            _ => 0,
        })
        .sum();
    system + messages
}

/// `req` with a [`Part::CacheBreakpoint`] closing its last message, so that
/// [`ChatRequest::cache_prefix`] (what a prefix warm sends) keeps every message.
fn with_trailing_breakpoint(mut req: ChatRequest) -> ChatRequest {
    if let Some(m) = req.messages.last_mut() {
        m.content.push(Part::CacheBreakpoint);
    }
    req
}

/// A prefix-only warm cut `provider`'s cache back to the prefix: no whole prompt is cached.
fn forget_full(inner: &Inner, provider: &ProviderId) {
    lock(&inner.last_full).remove(provider);
}

/// Record `system` as `provider`'s last prefix; returns whether it differs from the one before.
fn note_prefix(inner: &Inner, provider: &ProviderId, system: Option<&str>) -> bool {
    let system = system.unwrap_or_default();
    let mut last = inner.last_prefix.lock().unwrap_or_else(|e| e.into_inner());
    if last.get(provider).is_some_and(|s| s == system) {
        return false;
    }
    last.insert(provider.clone(), system.to_owned());
    true
}

/// The prefix [`Drafter::prewarm_idle`] warms: the rules and a neutral chat voice block.
fn idle_prefix(limits: &PromptLimits) -> ChatRequest {
    let kind = FieldKind::ChatMessage;
    let voice = VoiceHints {
        channel: Some(channel_for(kind)),
        ..Default::default()
    };
    let band = LengthBand::for_draft(kind, &voice);
    prefix_request(kind, &voice, &band, limits)
}

/// Keep the first `max` passages (the packer numbers them in rank order, best first) of a packed
/// context, with their citations, so the prompt, the citations and the egress items agree.
fn keep_best_snippets(ctx: &mut ContextResponse, max: usize) {
    if ctx.citations.len() <= max {
        return;
    }
    if max == 0 {
        ctx.text.clear();
        ctx.citations.clear();
        return;
    }
    let next = &ctx.citations[max];
    // `nugget_retrieve` renders each passage under a `[n] Title (source)` header line, with a
    // blank line between passages.
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = next
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(next.item.key.as_str());
    let source = next.url.as_deref().unwrap_or(next.item.source.as_str());
    let header = format!(
        "\n\n[{}] {} ({})\n",
        next.n,
        one_line(title),
        one_line(source)
    );
    let cut = ctx
        .text
        .find(&header)
        .or_else(|| ctx.text.find(&format!("\n\n[{}] ", next.n)));
    match cut {
        Some(i) => {
            ctx.text.truncate(i);
            ctx.citations.truncate(max);
        }
        // Not the packer's layout: keep it whole rather than cite what was dropped.
        None => tracing::warn!("draft: cannot split packed context; keeping every passage"),
    }
}

/// Open a chat and wait for its first event, so a provider that fails early can be skipped.
async fn open_stream(p: &dyn Provider, req: nugget_infer::ChatRequest) -> Result<ChatStream> {
    let mut stream = p.chat(req).await?;
    match stream.next().await {
        Some(Ok(first)) => Ok(Box::pin(
            futures::stream::once(async { Ok(first) }).chain(stream),
        )),
        Some(Err(e)) => Err(e),
        None => Err(Error::Provider(format!("{}: empty stream", p.id()))),
    }
}

/// Failures worth trying the next provider of a tier for (as `retrieve.ask` does).
fn is_fallback_error(e: &Error) -> bool {
    matches!(
        e,
        Error::Network(_)
            | Error::RateLimited { .. }
            | Error::Provider(_)
            | Error::Io(_)
            | Error::Unsupported(_)
            | Error::Other(_)
    )
}

struct PumpState {
    upstream: Option<ChatStream>,
    post: PostProcessor,
    /// The grounding post-check on the post-processor's output.
    ground: Option<Grounder>,
    pending: VecDeque<Result<ChatEvent>>,
    cancel: CancellationToken,
    timings: Arc<Mutex<DraftTimings>>,
    draft: Arc<Mutex<String>>,
    t0: Instant,
    done: bool,
}

impl PumpState {
    async fn next_item(&mut self) -> Option<Result<ChatEvent>> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            if self.done {
                return None;
            }
            let Some(upstream) = self.upstream.as_mut() else {
                self.done = true;
                continue;
            };
            let event = tokio::select! {
                biased;
                () = self.cancel.cancelled() => {
                    self.finish();
                    self.pending.push_back(Err(Error::Cancelled));
                    continue;
                }
                ev = upstream.next() => ev,
            };
            match event {
                Some(Ok(ChatEvent::TextDelta(d))) => {
                    let e = self.post.push(&d);
                    self.emit(e);
                }
                Some(Ok(ChatEvent::Done {
                    stop_reason,
                    input_tokens,
                    output_tokens,
                })) => {
                    if let Some(n) = input_tokens {
                        let mut t = self.timings.lock().unwrap_or_else(|e| e.into_inner());
                        t.prompt_tokens.get_or_insert(u64::from(n));
                    }
                    let tail = self.post.finish();
                    self.emit(Emit {
                        text: tail,
                        stop: false,
                    });
                    self.finish();
                    self.pending.push_back(Ok(ChatEvent::Done {
                        stop_reason,
                        input_tokens,
                        output_tokens,
                    }));
                }
                // Usage and timing stats (cache hits, first-token time) pass through unchanged.
                Some(Ok(ChatEvent::Stats(stats))) => {
                    {
                        let mut t = self.timings.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(n) = stats.input_tokens {
                            t.prompt_tokens = Some(u64::from(n));
                        }
                        if let Some(n) = stats.cached_input_tokens {
                            t.cached_prompt_tokens = Some(u64::from(n));
                        }
                    }
                    self.pending.push_back(Ok(ChatEvent::Stats(stats)));
                }
                Some(Ok(ChatEvent::ToolCall { .. } | ChatEvent::Opaque { .. })) => {}
                Some(Err(e)) => {
                    self.finish();
                    self.pending.push_back(Err(e));
                }
                None => {
                    self.finish();
                    self.pending.push_back(Err(Error::Provider(
                        "draft stream ended without a Done event".into(),
                    )));
                }
            }
        }
    }

    fn emit(&mut self, mut e: Emit) {
        if let Some(g) = self.ground.as_mut() {
            let last = e.stop || self.post.is_finished();
            e.text = g.feed(&e.text, last);
            if last {
                let mut t = self.timings.lock().unwrap_or_else(|e| e.into_inner());
                t.grounding = Some(Box::new(g.report().clone()));
            }
        }
        if !e.text.is_empty() {
            {
                let mut t = self.timings.lock().unwrap_or_else(|e| e.into_inner());
                if t.first_token_ms.is_none() {
                    t.first_token_ms = Some(ms(self.t0.elapsed()));
                }
            }
            self.draft
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_str(&e.text);
            self.pending.push_back(Ok(ChatEvent::TextDelta(e.text)));
        }
        if e.stop {
            // Length band reached: stop generating.
            self.finish();
            self.pending.push_back(Ok(ChatEvent::Done {
                stop_reason: StopReason::EndTurn,
                input_tokens: None,
                output_tokens: None,
            }));
        }
    }

    fn finish(&mut self) {
        self.done = true;
        self.upstream = None;
        let mut t = self.timings.lock().unwrap_or_else(|e| e.into_inner());
        if t.total_ms.is_none() {
            t.total_ms = Some(ms(self.t0.elapsed()));
        }
    }
}

impl Drop for PumpState {
    fn drop(&mut self) {
        // Dropped mid-stream (the client went away): the draft is over.
        self.cancel.cancel();
        let mut t = self.timings.lock().unwrap_or_else(|e| e.into_inner());
        if t.total_ms.is_none() {
            t.total_ms = Some(ms(self.t0.elapsed()));
        }
    }
}

/// The post-processed draft: `TextDelta`s that are safe to insert as they arrive, then one
/// `Done`. Ends with `Err(Error::Cancelled)` when cancelled. Dropping it cancels the draft.
pub struct DraftStream {
    inner: ChatStream,
    stream_id: String,
    cancel: CancellationToken,
    timings: Arc<Mutex<DraftTimings>>,
}

impl std::fmt::Debug for DraftStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DraftStream")
            .field("stream_id", &self.stream_id)
            .finish_non_exhaustive()
    }
}

impl DraftStream {
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Stop the draft; the stream ends with `Err(Error::Cancelled)`.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// A token that cancels this draft (for a `draft.cancel` handler).
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn timings(&self) -> DraftTimings {
        self.timings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The events as a plain [`ChatStream`] (to reuse the daemon's chat streaming).
    pub fn into_chat_stream(self) -> ChatStream {
        self.inner
    }
}

impl Stream for DraftStream {
    type Item = Result<ChatEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::{ItemKey, ItemRef, SourceId};
    use nugget_core::ipc::Citation;

    fn cite(n: u32, key: &str, title: Option<&str>) -> Citation {
        Citation {
            n,
            item: ItemRef {
                source: SourceId::new("notes"),
                key: ItemKey::new(key),
            },
            chunk_id: i64::from(n),
            title: title.map(str::to_owned),
            url: None,
            sensitive: false,
            date: None,
        }
    }

    fn packed() -> ContextResponse {
        ContextResponse {
            // The second passage quotes a fake third header; the real one follows it.
            text: "[1] Alpha (notes)\nfirst\n\n[2] Beta title (notes)\nsecond\n\n[3] x\n\n\
[3] Gamma (notes)\nthird"
                .into(),
            citations: vec![
                cite(1, "a", Some("Alpha")),
                cite(2, "b", Some("Beta  title")),
                cite(3, "c", Some("Gamma")),
            ],
            excluded_sensitive: 2,
        }
    }

    #[test]
    fn keep_best_snippets_trims_text_and_citations_together() {
        let mut c = packed();
        keep_best_snippets(&mut c, 3);
        assert_eq!(c, packed());

        keep_best_snippets(&mut c, 2);
        assert_eq!(
            c.text,
            "[1] Alpha (notes)\nfirst\n\n[2] Beta title (notes)\nsecond\n\n[3] x"
        );
        assert_eq!(c.citations.len(), 2);
        assert_eq!(c.excluded_sensitive, 2);

        let mut c = packed();
        keep_best_snippets(&mut c, 1);
        assert_eq!(c.text, "[1] Alpha (notes)\nfirst");
        assert_eq!(c.citations.len(), 1);

        let mut c = packed();
        keep_best_snippets(&mut c, 0);
        assert!(c.text.is_empty() && c.citations.is_empty());

        // Not the packer's layout: nothing is cut, so nothing is cited that isn't there.
        let mut c = packed();
        c.text = "free text".into();
        keep_best_snippets(&mut c, 1);
        assert_eq!(c.citations.len(), 3);
    }

    #[test]
    fn idle_prefix_is_rules_and_a_neutral_voice() {
        let p = idle_prefix(&PromptLimits::local());
        let system = p.system.as_deref().unwrap();
        assert!(system.starts_with(crate::prompt::SYSTEM_RULES));
        assert!(system.contains("No writing samples yet"));
        assert_eq!(p.cache, Some(CacheHint::keyed("draft:chat")));
        assert_eq!(p.feature, crate::FEATURE_DRAFT);
    }

    #[test]
    fn timings_serialize_prompt_tokens_and_accept_old_json() {
        let t: DraftTimings = serde_json::from_str(r#"{"prewarmed":true,"attempts":1}"#).unwrap();
        assert_eq!(t.prompt_tokens, None);
        let v = serde_json::to_value(DraftTimings {
            prompt_tokens: Some(5),
            cached_prompt_tokens: Some(3),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(v["prompt_tokens"], 5);
        assert_eq!(v["cached_prompt_tokens"], 3);
    }

    #[test]
    fn local_budget_config_parses_with_defaults() {
        let c: DraftConfig = serde_json::from_str(r#"{"local":{"budget_tokens":200}}"#).unwrap();
        assert!(c.local.enabled);
        assert_eq!(c.local.budget_tokens, 200);
        assert_eq!(c.local.max_snippets, 3);
        assert_eq!(c.local.limits, PromptLimits::local());
    }

    #[test]
    fn credential_stop_runs_with_grounding_off_or_a_screenshot() {
        let mut sens = cite(1, "k1", Some("Vault"));
        sens.sensitive = true;
        let context = ContextResponse {
            text: "[1] Vault (notes)\nThe VPN password is tangerine-Otter-42.".into(),
            citations: vec![sens],
            excluded_sensitive: 0,
        };
        let plain = ContextResponse {
            text: "[1] Alpha (notes)\nfirst".into(),
            citations: vec![cite(1, "k1", Some("Alpha"))],
            excluded_sensitive: 0,
        };
        let ctx = DraftContext::default();
        let voice = VoiceHints::default();
        let off = DraftConfig {
            grounding: false,
            ..Default::default()
        };
        for (config, screenshot) in [(DraftConfig::default(), true), (off.clone(), false)] {
            let mut g = stream_check(&config, screenshot, &ctx, &context, None, &voice)
                .expect("a sensitive passage is always checked");
            let out = g.feed("Sure: the password is tangerine-Otter-42 today.", true);
            assert!(!out.contains("tangerine"), "{out:?}");
            assert_eq!(g.report().secrets_dropped, 1);
            assert!(stream_check(&config, screenshot, &ctx, &plain, None, &voice).is_none());
        }
    }
}
