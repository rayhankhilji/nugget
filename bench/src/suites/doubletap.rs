//! `bench/doubletap`: Double Tap latency through the real drafting engine (milestone M3 exit
//! criterion: time to the first insertable token).
//!
//! Every draft goes through [`nugget_draft::Drafter`], built as `nuggetd` builds it
//! (`crates/nuggetd/src/features/draft.rs`): the default [`DraftConfig`] (Synthesis tier; the
//! smaller [`nugget_draft::LocalBudget`] for non-cloud providers unless `--draft-local-budget
//! off`; temperature 0 and a fixed `seed` in the request body for real OpenAI-compatible
//! providers, so runs are repeatable), a provider warmer that calls [`Provider::prewarm`] and
//! [`Provider::prewarm_prefix`] (the draft's stable prefix into the prompt cache), retrieval run on
//! a blocking thread, over one in-memory [`Store`] per persona holding the `draft` fixture
//! corpus (`bench/fixtures/draft/`: every knowledge item, sensitive ones flagged, plus the
//! persona's sent messages). Retrieval is **text-only** (no embedder). Voice hints come from the
//! persona's sent messages (channel or recipient style statistics plus three exemplars), so the
//! prompt has the size and shape of a real one.
//!
//! Once at the start, untimed, as the daemon does when Double Tap is enabled:
//! [`Drafter::prewarm_idle`] (the fixed rules prefix), waited for. Then per task, as the
//! desktop shell does on a trigger (`apps/desktop/src-tauri/src/doubletap/
//! flow.rs`): [`Drafter::prewarm`] with an app-only hint (the shell prewarms before it has read
//! the field and thread, so retrieval is not started early), then [`Drafter::begin`] with the
//! task's full [`DraftContext`], then the [`nugget_draft::DraftStream`] is drained and its
//! [`DraftTimings`] read. Times are measured from the start of `begin`, and again from the
//! trigger (`from_trigger.*`, from the `prewarm` call); the accessibility read of the field and
//! the IPC hop to the daemon are not included.
//!
//! Metrics (seconds unless noted, `doubletap.` prefix):
//! - `ttft.p50/.p95`: until the provider's first raw stream event (`provider_open_ms`).
//!   Prompt building, retrieval and voice lookup are included; for a hybrid reasoning model
//!   whose thinking the adapter drops, this is the first visible event after the thinking.
//! - `first_inserted_token.p50/.p95`: until the first post-processed text the engine emits
//!   (`first_token_ms`). In the product that text streams into the caret overlay and is
//!   inserted into the field when the user accepts, so this is the first token the user can
//!   insert. The accessibility insertion itself is not timed here. The M3 target
//!   ([`TTFT_TARGET_LOCAL_S`] on-device, [`TTFT_TARGET_CLOUD_S`] otherwise) applies to its
//!   p50 for real providers.
//! - `first_inserted_char.p50/.p95`: until the stream emitted its first non-whitespace
//!   character (what the overlay inserts first), measured by the bench.
//! - `total.p50/.p95`: until the stream finished.
//! - `from_trigger.{ttft,first_inserted_token,first_inserted_char,total}.p50/.p95`: the same,
//!   from the trigger, so time spent in a prefix prewarm cannot hide.
//! - `prompt_tokens.p50/.p95`, `cached_prompt_tokens.p50/.p95` (served from the provider's
//!   prompt cache, llama-server `cache_n`) and `evaluated_prompt_tokens.p50/.p95` (prompt minus
//!   cached: what prefill time grows with), when the provider reports them.
//! - `retrieval.p50`: how long `begin` waited for retrieval and the voice lookup.
//! - `requests`, `completed`, `failures`, `timeouts`, `empty_drafts` (completed with no visible
//!   text), `retrieval_skipped` (retrieval timed out or failed, so the draft had no context):
//!   counts. Percentiles cover completed drafts only; failed and timed-out drafts are counted,
//!   never hidden. The suite fails only when no draft completed.
//! - `output_tokens_per_s` (provider-reported output tokens over the time after the first
//!   insertable token) and `prompt_tokens.mean` (provider-reported), when the provider reports
//!   usage.
//! - `cold.*`: one untimed warm-up draft first (model load, first KV fill), reported apart.
//! - `cache_reuse.*`: after the main pass, one task drafted twice with `begin` alone
//!   (`repeat_first`, `repeat_second`: the second should read only the new tokens,
//!   `repeat_second.evaluated_share` near 0) and a third time in the product flow
//!   (`repeat_after_prewarm`); then another task after a cache reset (a tiny unrelated
//!   request) with `begin` alone (`no_prewarm`) and after another reset with the `prewarm`
//!   waited for (`prewarmed`, with `prewarm_wait` and `from_trigger.first_inserted_token`).
//!   `prewarm_saved_tokens` is `no_prewarm` minus `prewarmed` evaluated tokens. Each arm
//!   reports the token counts above and the latencies from `begin`.
//! - `thinking_model_default.*` (`--local-models` only): [`THINKING_TASKS`] drafts with the
//!   model's own thinking default ([`Thinking::Model`]), showing what Qwen3's default thinking
//!   costs, and how many drafts it left without visible text.
//!
//! - `server_cpu_s.mean` (`--local-models` only): CPU seconds the `llama-server` process spent
//!   per timed draft, trigger to end of stream.
//!
//! **Read-ahead** (`--doubletap-read-ahead on`; default `off`, so earlier results stay
//! comparable): as when a reply field keeps focus before the user taps, every task of the
//! warm-up and timed passes first calls [`Drafter::read_ahead`] with the same [`DraftRequest`]
//! the trigger sends and waits for it (bounded by the per-draft timeout; cancelled with
//! [`Drafter::cancel_read_ahead`] on timeout), then runs the trigger flow above unchanged, so
//! `from_trigger.first_inserted_char` and the cached/evaluated token counts show what the
//! read-ahead saved at the trigger. The bench sets `[draft.read_ahead] min_interval_ms = 0`
//! (its tasks stand for focus changes minutes apart; the projection below applies the product's
//! interval). Extra metrics (`doubletap.read_ahead.` prefix, timed pass only):
//! - `requests`, `completed`, `timeouts`, `skipped` and `skipped.<reason>`
//!   ([`nugget_draft::ReadAheadSkip::as_str`]): `skipped` must be 0 with a local provider (its target on real
//!   on-device runs).
//! - `time.p50/.p95`: wall-clock time of a completed read-ahead (what the user's focus has to
//!   last for the trigger to find the prompt cached); `prepare.p50` and `warm.p50`: its retrieval
//!   and prefill parts ([`ReadAheadOutcome`]).
//! - `server_cpu_s.mean/.p50` (`--local-models` only): CPU seconds `llama-server` spent per
//!   completed read-ahead (`/proc/<pid>/stat` on Linux, `ps -o cputime` on macOS), and
//!   `core_pct.at_{1,2,4,6}_per_min`: that cost as a share of one core at that many focus changes
//!   per minute (each one reading a new context ahead; capped by the product's
//!   `min_interval_ms`). Battery drain cannot be metered on CI runners: these are CPU-seconds,
//!   the input to any energy estimate, not watt-hours.
//!
//! Models: `--local-models` runs the signed manifest's smallest chat model through
//! `nugget-runtime` ([`LocalModels`]); else `--provider-url` (with `--provider-model`,
//! `--provider-kind`, `--api-key-env`, as for `draft`); else a deterministic in-process fake
//! provider whose latency comes from `--fake-ttft-ms`, `--fake-prefill-ms-per-1k` and
//! `--fake-token-ms`, and whose locality from `--draft-locality`, so the suite runs in CI. The
//! fake simulates a one-slot prompt cache (longest common prefix with the previous prompt or
//! with a prewarmed prefix; prefill time only for uncached tokens) and reports it in `Stats`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use nugget_core::capability::{Locality, Tier};
use nugget_core::egress::NullEgress;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{DraftContext, DraftRequest, RuntimeRole};
use nugget_core::{Error, Result as CoreResult};
use nugget_draft::{
    DraftConfig, DraftTimings, Drafter, ReadAheadConfig, ReadAheadOutcome, RetrievalRunner,
    VoiceQuery, VoiceSource, Warmer,
};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStats, ChatStream, Message, Part, Provider, ProviderConfig,
    ProviderKind, ProviderMeta, Router, StopReason, build_provider, build_provider_with,
};
use nugget_retrieve::{PackedContext, RetrieveConfig, Retriever};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use nugget_voice::{Channel, VoiceHints};
use serde_json::json;
use time::{Date, OffsetDateTime, Time};

use crate::local::{HostInfo, LocalModels, Thinking, process_cpu_seconds};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, Benchmark, RunContext};
use crate::stats::summarize;
use crate::suites::draft::fixtures::{Corpus, Persona, Task};
use crate::suites::draft::{
    DraftLocality, DraftOptions, Switch, TTFT_TARGET_CLOUD_S, TTFT_TARGET_LOCAL_S, style,
};

/// `doubletap` options (flattened into the CLI).
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct DoubletapOptions {
    /// `doubletap`: before each trigger, read the draft's whole prompt ahead
    /// (`Drafter::read_ahead`, as when a reply field keeps focus) and wait for it (`on`), or not
    /// (`off`, the default).
    #[arg(long, value_enum, default_value_t = Switch::Off)]
    pub doubletap_read_ahead: Switch,
    /// `doubletap`: grounding (`DraftConfig::grounding`; facts block and streaming check, see
    /// `--draft-grounding-check`): `on` (the product default) or `off`.
    #[arg(long, value_enum, default_value_t = Switch::On)]
    pub doubletap_grounding: Switch,
}

impl Default for DoubletapOptions {
    fn default() -> Self {
        Self {
            doubletap_read_ahead: Switch::Off,
            doubletap_grounding: Switch::On,
        }
    }
}

/// Focus changes per minute the read-ahead CPU cost is projected at.
pub const FOCUS_CHANGES_PER_MIN: [u32; 4] = [1, 2, 4, 6];

/// Longest a single draft may take (prewarm to end of stream) before it counts as timed out.
pub const DRAFT_TIMEOUT: Duration = Duration::from_secs(180);
/// Drafts in the `--local-models` thinking pass.
pub const THINKING_TASKS: usize = 3;
/// Exemplars in the persona voice hints.
const EXEMPLARS: usize = 3;
/// A recipient needs this many messages on the channel for recipient-specific statistics.
const MIN_RECIPIENT_SAMPLES: usize = 3;
/// Tier the drafter routes by (its default), and the only tier the bench router serves.
const TIER: Tier = Tier::Synthesis;
/// What the fake provider answers (post-processed like any reply).
const FAKE_REPLY: &str = "Thanks for checking. That works for me, and I can confirm the details \
     with the team by Friday. Let me know if anything changes on your side.";

const KNOWLEDGE_SOURCE: &str = "knowledge";
const SENT_SOURCE: &str = "sent";

pub const RETRIEVAL_DESC: &str = "nugget-retrieve text-only (no embedder), default RetrieveConfig, \
     fixed reference time; one in-memory store per persona (draft fixture knowledge items + the \
     persona's sent messages)";
pub const PREWARM_DESC: &str = "Drafter::prewarm with an app-only hint before begin, as \
     apps/desktop doubletap/flow.rs (connection warm-up; retrieval starts in begin)";
pub const VOICE_DESC: &str = "persona sent messages: recipient (>= 3 messages) or channel style \
     statistics + 3 exemplars (recipient first, then most recent)";

// ---------------------------------------------------------------------------------------------
// Fake provider

/// Latency of the in-process fake provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FakeLatency {
    pub ttft_ms: u64,
    pub prefill_ms_per_1k: u64,
    pub token_ms: u64,
}

/// A deterministic chat provider: [`FAKE_REPLY`] word by word after a latency that grows with
/// the prompt tokens it has to read, with usage in `Stats` and `Done`. It simulates
/// llama-server's prompt cache (one slot): the tokens of the longest common prefix with the
/// previous prompt (or with a prefix warmed by [`Provider::prewarm_prefix`]) are served from the
/// cache, reported as `cached_input_tokens`, and cost no prefill time. Can also fail or stall on
/// request (tests).
pub struct FakeChat {
    id: ProviderId,
    locality: Locality,
    latency: FakeLatency,
    /// Every `n`-th request (1-based) fails before streaming.
    fail_every: Option<usize>,
    /// Every request opens a stream that never yields.
    stall: bool,
    requests: AtomicUsize,
    /// The slot's cached prompt text. Held across a prefix prewarm's prefill, so a request
    /// that arrives meanwhile queues behind it (as on a one-slot server).
    slot: tokio::sync::Mutex<String>,
}

impl FakeChat {
    pub fn new(locality: Locality, latency: FakeLatency) -> Self {
        Self {
            id: ProviderId::new("bench-doubletap-fake"),
            locality,
            latency,
            fail_every: None,
            stall: false,
            requests: AtomicUsize::new(0),
            slot: tokio::sync::Mutex::new(String::new()),
        }
    }

    pub fn failing_every(mut self, n: usize) -> Self {
        self.fail_every = Some(n.max(1));
        self
    }

    pub fn stalling(mut self) -> Self {
        self.stall = true;
        self
    }

    fn prefill(&self, uncached: usize) -> Duration {
        Duration::from_millis(self.latency.prefill_ms_per_1k * uncached as u64 / 1000)
    }
}

/// A request's prompt as the fake model reads it: system, then each message's role and text
/// parts in order, with nothing closing a message, so a request's [`ChatRequest::cache_prefix`]
/// renders to a prefix of the request's own rendering.
fn render_prompt(req: &ChatRequest) -> String {
    let mut out = String::new();
    if let Some(s) = &req.system {
        out.push_str("<system>\n");
        out.push_str(s);
    }
    for m in &req.messages {
        out.push_str(&format!("\n<{:?}>\n", m.role));
        for p in &m.content {
            if let Part::Text { text } = p {
                out.push_str(text);
            }
        }
    }
    out
}

/// Byte length of the longest common prefix of `a` and `b`, on a char boundary.
fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or_else(|| a.len().min(b.len()), |((i, _), _)| i)
}

/// Estimated tokens of a prompt text.
fn tokens(text: &str) -> usize {
    nugget_extract::estimate_tokens(text)
}

#[async_trait]
impl Provider for FakeChat {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "fake-deterministic".into(),
            locality: self.locality,
            context_tokens: None,
        }
    }

    async fn prewarm_prefix(&self, req: &ChatRequest) -> CoreResult<()> {
        let Some(prefix) = req.cache_prefix() else {
            return Ok(());
        };
        let text = render_prompt(&prefix);
        let mut slot = self.slot.lock().await;
        let cached = tokens(&text[..common_prefix(&text, &slot)]);
        let delay = self.prefill(tokens(&text).saturating_sub(cached));
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        *slot = text;
        Ok(())
    }

    async fn chat(&self, req: ChatRequest) -> CoreResult<ChatStream> {
        let n = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_every.is_some_and(|k| n.is_multiple_of(k)) {
            return Err(Error::Provider("fake provider: injected failure".into()));
        }
        if self.stall {
            return Ok(futures::stream::pending().boxed());
        }
        let text = render_prompt(&req);
        let (input, cached) = {
            let mut slot = self.slot.lock().await;
            let input = tokens(&text);
            let cached = tokens(&text[..common_prefix(&text, &slot)]).min(input);
            *slot = text;
            (input, cached)
        };
        let first = Duration::from_millis(self.latency.ttft_ms)
            + self.prefill(input.saturating_sub(cached));
        let per_token = Duration::from_millis(self.latency.token_ms);
        let words: Vec<String> = FAKE_REPLY.split_inclusive(' ').map(str::to_owned).collect();
        let output = u32::try_from(words.len()).unwrap_or(u32::MAX);
        let input = u32::try_from(input).unwrap_or(u32::MAX);
        let cached = u32::try_from(cached).unwrap_or(u32::MAX);
        let stream = futures::stream::unfold(0usize, move |i| {
            let words = words.clone();
            async move {
                if i > words.len() + 1 {
                    return None;
                }
                let delay = if i == 0 {
                    first
                } else if i < words.len() {
                    per_token
                } else {
                    Duration::ZERO
                };
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let ev = match words.get(i) {
                    Some(w) => ChatEvent::TextDelta(w.clone()),
                    None if i == words.len() => ChatEvent::Stats(ChatStats {
                        input_tokens: Some(input),
                        output_tokens: Some(output),
                        cached_input_tokens: Some(cached),
                        ..ChatStats::default()
                    }),
                    None => ChatEvent::Done {
                        stop_reason: StopReason::EndTurn,
                        input_tokens: Some(input),
                        output_tokens: Some(output),
                    },
                };
                Some((Ok(ev), i + 1))
            }
        });
        Ok(stream.boxed())
    }

    async fn embed(&self, _texts: &[String]) -> CoreResult<Vec<Vec<f32>>> {
        Err(Error::Unsupported(
            "fake chat provider has no embeddings".into(),
        ))
    }
}

// ---------------------------------------------------------------------------------------------
// The drafter, as the daemon builds it

/// Opens the provider's connection on trigger and warms its prompt cache with the draft's
/// stable prefix (as `nuggetd`'s `ProviderWarmer`). Counts the warm-ups it started and
/// finished, so the bench can wait for them (bounded) where a pass needs a finished prewarm.
#[derive(Default)]
pub struct ProviderWarmer {
    started: AtomicUsize,
    finished: AtomicUsize,
    /// `warm_prefix` calls (the engine handed over a prefix to cache).
    prefix_calls: AtomicUsize,
}

impl ProviderWarmer {
    pub fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }

    pub fn prefix_calls(&self) -> usize {
        self.prefix_calls.load(Ordering::SeqCst)
    }

    /// Wait until at least `at_least` warm-ups have started and every started one finished.
    /// False on timeout.
    pub async fn settle(&self, at_least: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let started = self.started();
            if started >= at_least && self.finished.load(Ordering::SeqCst) >= started {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}

#[async_trait]
impl Warmer for ProviderWarmer {
    async fn warm(&self, provider: Arc<dyn Provider>) {
        self.started.fetch_add(1, Ordering::SeqCst);
        let _ = provider.prewarm().await;
        self.finished.fetch_add(1, Ordering::SeqCst);
    }

    async fn warm_prefix(&self, provider: Arc<dyn Provider>, prefix: ChatRequest) {
        self.started.fetch_add(1, Ordering::SeqCst);
        self.prefix_calls.fetch_add(1, Ordering::SeqCst);
        let _ = provider.prewarm_prefix(&prefix).await;
        self.finished.fetch_add(1, Ordering::SeqCst);
    }
}

/// Runs retrieval on a blocking-pool thread (as `nuggetd`'s `BlockingRetrieval`).
struct BlockingRetrieval;

#[async_trait]
impl RetrievalRunner for BlockingRetrieval {
    async fn run(
        &self,
        job: BoxFuture<'static, CoreResult<PackedContext>>,
    ) -> CoreResult<PackedContext> {
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || handle.block_on(job))
            .await
            .map_err(|e| Error::Other(format!("retrieval task failed: {e}")))?
    }
}

/// Voice hints from a persona's sent messages.
struct PersonaVoice {
    persona: Persona,
}

#[async_trait]
impl VoiceSource for PersonaVoice {
    async fn hints(&self, query: &VoiceQuery) -> CoreResult<VoiceHints> {
        let channel = match query.channel {
            Channel::Chat | Channel::Comment => "chat",
            _ => "email",
        };
        let recipients: Vec<String> = query
            .recipients
            .iter()
            .map(|r| r.to_ascii_lowercase())
            .collect();
        let to_them = |to: &str| {
            let to = to.to_ascii_lowercase();
            recipients.iter().any(|r| r.contains(&to))
        };
        let on_channel: Vec<_> = self
            .persona
            .sent
            .iter()
            .filter(|m| m.channel == channel)
            .collect();
        let theirs: Vec<_> = on_channel
            .iter()
            .filter(|m| to_them(&m.to))
            .copied()
            .collect();
        let basis = if theirs.len() >= MIN_RECIPIENT_SAMPLES {
            &theirs
        } else {
            &on_channel
        };
        let stats = style::stats(basis.iter().map(|m| m.text.as_str()));
        let mut ranked = on_channel.clone();
        ranked.sort_by_key(|m| {
            (
                std::cmp::Reverse(to_them(&m.to)),
                std::cmp::Reverse(m.date.clone()),
            )
        });
        Ok(VoiceHints {
            channel: Some(query.channel),
            stats,
            exemplars: ranked
                .iter()
                .take(EXEMPLARS)
                .map(|m| m.text.clone())
                .collect(),
        })
    }
}

/// `YYYY-MM-DD` at midnight UTC.
fn parse_date(d: &str) -> Result<OffsetDateTime, BenchError> {
    let bad = || BenchError::Failed(format!("bad fixture date {d}"));
    let mut it = d.split('-').map(str::parse::<i32>);
    let (Some(Ok(y)), Some(Ok(m)), Some(Ok(day)), None) =
        (it.next(), it.next(), it.next(), it.next())
    else {
        return Err(bad());
    };
    let month = time::Month::try_from(u8::try_from(m).map_err(|_| bad())?).map_err(|_| bad())?;
    let day = u8::try_from(day).map_err(|_| bad())?;
    Date::from_calendar_date(y, month, day)
        .map(|d| d.with_time(Time::MIDNIGHT).assume_utc())
        .map_err(|_| bad())
}

/// One store per persona: every knowledge item (sensitive ones flagged) and the persona's sent
/// messages, one chunk per item, no embeddings.
pub(crate) fn build_stores(corpus: &Corpus) -> Result<BTreeMap<String, Store>, BenchError> {
    let mut stores = BTreeMap::new();
    for p in &corpus.personas {
        let store = Store::open_in_memory()?;
        for it in &corpus.items {
            let item = NewItem::new(KNOWLEDGE_SOURCE, it.id.clone(), content_hash(&it.body))
                .with_title(it.title.clone())
                .with_mime("text/plain")
                .with_metadata(json!({ "sensitive": it.sensitive, "kind": it.kind }))
                .with_modified_at(parse_date(&it.date)?);
            store.upsert_item(&item)?;
            store.replace_chunks(&item.item, &[NewChunk::text(it.body.clone())])?;
        }
        for m in &p.sent {
            let item = NewItem::new(SENT_SOURCE, m.id.clone(), content_hash(&m.text))
                .with_title(format!("Message to {}", m.to_name))
                .with_mime("text/plain")
                .with_metadata(json!({ "sensitive": false }))
                .with_modified_at(parse_date(&m.date)?);
            store.upsert_item(&item)?;
            store.replace_chunks(&item.item, &[NewChunk::text(m.text.clone())])?;
        }
        stores.insert(p.id.clone(), store);
    }
    Ok(stores)
}

/// One drafter per persona (all routing to one provider on the Synthesis tier), sharing one
/// warmer.
pub struct Drafters {
    by_persona: HashMap<String, Drafter>,
    warmer: Arc<ProviderWarmer>,
}

impl Drafters {
    fn get(&self, persona: &str) -> Option<&Drafter> {
        self.by_persona.get(persona)
    }

    /// What the daemon does when Double Tap is enabled: [`Drafter::prewarm_idle`] once (every
    /// drafter shares the provider and the rules prefix), then wait for it, bounded. Returns
    /// the provider warmed and whether the warm-up finished in time.
    pub(crate) async fn prewarm_idle(&self, wait: Duration) -> Option<(ProviderId, bool)> {
        let mut ids: Vec<&String> = self.by_persona.keys().collect();
        ids.sort();
        let drafter = self.by_persona.get(*ids.first()?)?;
        let before = self.warmer.started();
        let provider = drafter.prewarm_idle()?;
        let done = self.warmer.settle(before + 1, wait).await;
        Some((provider, done))
    }
}

pub(crate) fn build_drafters(
    corpus: &Corpus,
    stores: &BTreeMap<String, Store>,
    provider: Arc<dyn Provider>,
    config: &DraftConfig,
) -> Drafters {
    let tiers = HashMap::from([(TIER, vec![provider.id().clone()])]);
    let router = Arc::new(Router::new(vec![provider], tiers));
    let now = crate::suites::draft::arms::reference_time();
    let warmer = Arc::new(ProviderWarmer::default());
    let mut out = HashMap::new();
    for p in &corpus.personas {
        let Some(store) = stores.get(&p.id) else {
            continue;
        };
        let retriever =
            Retriever::new(store.clone(), None, RetrieveConfig::default()).with_reference_time(now);
        let drafter = Drafter::new(
            store.clone(),
            retriever,
            router.clone(),
            Arc::new(PersonaVoice { persona: p.clone() }),
            config.clone(),
        )
        .with_warmer(warmer.clone())
        .with_retrieval_runner(Arc::new(BlockingRetrieval));
        out.insert(p.id.clone(), drafter);
    }
    Drafters {
        by_persona: out,
        warmer,
    }
}

// ---------------------------------------------------------------------------------------------
// Running drafts

/// One completed draft.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    pub timings: DraftTimings,
    pub text_chars: usize,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    /// Prompt tokens the provider served from its cache (from its `Stats`).
    pub cached_tokens: Option<u32>,
    /// From the start of `begin` until the first non-whitespace character the stream emitted
    /// (what the overlay inserts first), measured by the bench.
    pub first_char_ms: Option<u64>,
    /// From the trigger (the `prewarm` call) to the start of `begin`: zero but for the call
    /// itself in the product flow, the wait in [`Flow::WaitForPrewarm`].
    pub lead_ms: u64,
    /// [`Flow::WaitForPrewarm`]: whether the prewarm finished within the wait.
    pub prewarm_settled: Option<bool>,
    /// CPU seconds the local `llama-server` spent from the trigger to the end of the stream
    /// (`--local-models` only).
    pub server_cpu_s: Option<f64>,
}

impl Sample {
    /// Prompt tokens of the request, cached or not: the engine's timings, else the stream's
    /// usage.
    pub fn prompt_tokens(&self) -> Option<u64> {
        self.timings
            .prompt_tokens
            .or(self.input_tokens.map(u64::from))
    }

    pub fn cached_prompt_tokens(&self) -> Option<u64> {
        self.timings
            .cached_prompt_tokens
            .or(self.cached_tokens.map(u64::from))
    }

    /// Prompt tokens the provider actually read (prompt minus cached); needs both.
    pub fn evaluated_prompt_tokens(&self) -> Option<u64> {
        Some(
            self.prompt_tokens()?
                .saturating_sub(self.cached_prompt_tokens()?),
        )
    }

    /// `ms` (from `begin`) shifted to be from the trigger.
    fn since_trigger(&self, ms: Option<u64>) -> Option<u64> {
        ms.map(|m| m + self.lead_ms)
    }
}

/// How one draft ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Completed(Sample),
    Failed(String),
    TimedOut,
}

impl Outcome {
    pub fn sample(&self) -> Option<&Sample> {
        match self {
            Self::Completed(s) => Some(s),
            _ => None,
        }
    }
}

/// What happens between the trigger and `begin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// As the shell: `prewarm` with an app-only hint, then `begin` straight away.
    Product,
    /// `begin` alone (no prewarm).
    NoPrewarm,
    /// `prewarm`, then wait (bounded) until the warm-up it started finished, then `begin`.
    WaitForPrewarm(Duration),
}

/// Trigger, begin, drain. Bounded by `timeout` end to end (a prewarm wait included).
async fn one_draft(
    drafters: &Drafters,
    drafter: &Drafter,
    task: &Task,
    timeout: Duration,
    flow: Flow,
) -> Outcome {
    let hint = DraftContext {
        app: task.context.app.clone(),
        ..DraftContext::default()
    };
    let req = draft_request(task);
    let work = async {
        let trigger = Instant::now();
        let mut settled = None;
        match flow {
            Flow::Product => {
                drafter.prewarm(&hint);
            }
            Flow::NoPrewarm => {}
            Flow::WaitForPrewarm(wait) => {
                let before = drafters.warmer.started();
                if drafter.prewarm(&hint).is_some() {
                    settled = Some(drafters.warmer.settle(before + 1, wait).await);
                }
            }
        }
        let begin = Instant::now();
        let (_, mut stream) = drafter.begin(req).await?;
        let mut sample = Sample {
            lead_ms: ms_between(trigger, begin),
            prewarm_settled: settled,
            ..Sample::default()
        };
        while let Some(ev) = stream.next().await {
            match ev? {
                ChatEvent::TextDelta(d) => {
                    if sample.first_char_ms.is_none() && d.chars().any(|c| !c.is_whitespace()) {
                        sample.first_char_ms = Some(ms_between(begin, Instant::now()));
                    }
                    sample.text_chars += d.chars().count();
                }
                ChatEvent::Stats(s) => {
                    sample.input_tokens = sample.input_tokens.or(s.input_tokens);
                    sample.output_tokens = sample.output_tokens.or(s.output_tokens);
                    sample.cached_tokens = sample.cached_tokens.or(s.cached_input_tokens);
                }
                ChatEvent::Done {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    sample.input_tokens = input_tokens.or(sample.input_tokens);
                    sample.output_tokens = output_tokens.or(sample.output_tokens);
                    break;
                }
                _ => {}
            }
        }
        sample.timings = stream.timings();
        Ok::<_, Error>(sample)
    };
    match tokio::time::timeout(timeout, work).await {
        Err(_) => Outcome::TimedOut,
        Ok(Err(e)) => Outcome::Failed(e.to_string()),
        Ok(Ok(s)) => Outcome::Completed(s),
    }
}

/// The request the trigger sends for `task` (and a read-ahead reads ahead).
fn draft_request(task: &Task) -> DraftRequest {
    DraftRequest {
        context: task.context.clone(),
        instruction: None,
        provider: None,
        max_tokens: None,
    }
}

/// How one read-ahead ended.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadAheadResult {
    Done {
        /// Wall-clock time of the `read_ahead` call.
        wall_ms: u64,
        outcome: ReadAheadOutcome,
        /// CPU seconds `llama-server` spent meanwhile (`--local-models` only).
        server_cpu_s: Option<f64>,
    },
    Skipped {
        /// [`nugget_draft::ReadAheadSkip::as_str`], or `no_drafter`.
        reason: &'static str,
    },
    TimedOut,
}

/// What a pass does besides the trigger flow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Probe {
    /// Read each task ahead ([`Drafter::read_ahead`], waited for) before its trigger.
    pub read_ahead: bool,
    /// The local server to meter CPU time of.
    pub server_pid: Option<u32>,
}

impl Probe {
    fn cpu(&self) -> Option<f64> {
        self.server_pid.and_then(process_cpu_seconds)
    }
}

/// Read `task` ahead with the request its trigger will send, bounded by `timeout` (cancelled
/// when it runs out).
pub(crate) async fn read_ahead_task(
    drafters: &Drafters,
    task: &Task,
    timeout: Duration,
    probe: &Probe,
) -> ReadAheadResult {
    let Some(drafter) = drafters.get(&task.persona) else {
        return ReadAheadResult::Skipped {
            reason: "no_drafter",
        };
    };
    let cpu_before = probe.cpu();
    let started = Instant::now();
    let res = tokio::time::timeout(timeout, drafter.read_ahead(draft_request(task))).await;
    let wall_ms = ms_between(started, Instant::now());
    match res {
        Err(_) => {
            drafter.cancel_read_ahead();
            ReadAheadResult::TimedOut
        }
        Ok(Err(skip)) => ReadAheadResult::Skipped {
            reason: skip.as_str(),
        },
        Ok(Ok(outcome)) => ReadAheadResult::Done {
            wall_ms,
            outcome,
            server_cpu_s: cpu_before.zip(probe.cpu()).map(|(a, b)| (b - a).max(0.0)),
        },
    }
}

fn ms_between(from: Instant, to: Instant) -> u64 {
    u64::try_from(to.saturating_duration_since(from).as_millis()).unwrap_or(u64::MAX)
}

/// Every draft of one pass, in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pass {
    pub outcomes: Vec<Outcome>,
    /// With [`Probe::read_ahead`]: the read-ahead before each draft, in the same order.
    pub read_aheads: Vec<ReadAheadResult>,
}

impl Pass {
    pub fn samples(&self) -> impl Iterator<Item = &Sample> {
        self.outcomes.iter().filter_map(Outcome::sample)
    }

    pub fn completed(&self) -> usize {
        self.samples().count()
    }

    pub fn failures(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| matches!(o, Outcome::Failed(_)))
            .count()
    }

    pub fn timeouts(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| matches!(o, Outcome::TimedOut))
            .count()
    }

    pub fn empty(&self) -> usize {
        self.samples().filter(|s| s.text_chars == 0).count()
    }

    pub fn first_error(&self) -> Option<&str> {
        self.outcomes.iter().find_map(|o| match o {
            Outcome::Failed(e) => Some(e.as_str()),
            Outcome::TimedOut => Some("timed out"),
            Outcome::Completed(_) => None,
        })
    }
}

/// Draft one task with `flow`.
async fn draft_task(drafters: &Drafters, task: &Task, timeout: Duration, flow: Flow) -> Outcome {
    match drafters.get(&task.persona) {
        Some(d) => one_draft(drafters, d, task, timeout, flow).await,
        None => Outcome::Failed(format!("no drafter for persona {}", task.persona)),
    }
}

async fn run_pass(
    drafters: &Drafters,
    corpus: &Corpus,
    tasks: &[usize],
    timeout: Duration,
    label: &str,
    probe: Probe,
) -> Pass {
    let mut pass = Pass::default();
    for (n, &ti) in tasks.iter().enumerate() {
        let task = &corpus.tasks[ti];
        if probe.read_ahead {
            let ra = read_ahead_task(drafters, task, timeout, &probe).await;
            eprintln!(
                "doubletap {label}: {}/{} {} read-ahead -> {}",
                n + 1,
                tasks.len(),
                task.id,
                describe_read_ahead(&ra)
            );
            pass.read_aheads.push(ra);
        }
        let cpu_before = probe.cpu();
        let mut outcome = draft_task(drafters, task, timeout, Flow::Product).await;
        if let Outcome::Completed(s) = &mut outcome {
            s.server_cpu_s = cpu_before.zip(probe.cpu()).map(|(a, b)| (b - a).max(0.0));
        }
        eprintln!(
            "doubletap {label}: {}/{} {} -> {}",
            n + 1,
            tasks.len(),
            task.id,
            describe(&outcome)
        );
        pass.outcomes.push(outcome);
    }
    pass
}

fn describe_read_ahead(r: &ReadAheadResult) -> String {
    match r {
        ReadAheadResult::Done {
            wall_ms,
            outcome,
            server_cpu_s,
        } => format!(
            "done in {wall_ms} ms (prepare {} ms, prefill {} ms, {} prompt chars{})",
            outcome.prepare_ms,
            outcome.warm_ms,
            outcome.prompt_chars,
            server_cpu_s.map_or_else(String::new, |c| format!(", server CPU {c:.2} s"))
        ),
        ReadAheadResult::Skipped { reason } => format!("SKIPPED: {reason}"),
        ReadAheadResult::TimedOut => "TIMED OUT".into(),
    }
}

fn describe(o: &Outcome) -> String {
    match o {
        Outcome::Completed(s) => format!(
            "first inserted token {}, first char {}, total {}, prompt {} tokens ({} cached), {} chars",
            s.timings
                .first_token_ms
                .map_or_else(|| "none".into(), |ms| format!("{ms} ms")),
            s.first_char_ms
                .map_or_else(|| "none".into(), |ms| format!("{ms} ms")),
            s.timings
                .total_ms
                .map_or_else(|| "?".into(), |ms| format!("{ms} ms")),
            s.prompt_tokens()
                .map_or_else(|| "?".into(), |n| n.to_string()),
            s.cached_prompt_tokens()
                .map_or_else(|| "?".into(), |n| n.to_string()),
            s.text_chars
        ),
        Outcome::Failed(e) => format!("FAILED: {e}"),
        Outcome::TimedOut => "TIMED OUT".into(),
    }
}

// ---------------------------------------------------------------------------------------------
// Metrics

fn secs(ms: Option<u64>) -> Option<f64> {
    ms.map(|m| m as f64 / 1000.0)
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The M3 target for time to the first insertable token, by provider locality.
pub fn latency_target(locality: Locality) -> f64 {
    if locality == Locality::OnDevice {
        TTFT_TARGET_LOCAL_S
    } else {
        TTFT_TARGET_CLOUD_S
    }
}

fn values(pass: &Pass, f: impl Fn(&Sample) -> Option<u64>) -> Vec<f64> {
    pass.samples().filter_map(|s| secs(f(s))).collect()
}

fn token_values(pass: &Pass, f: impl Fn(&Sample) -> Option<u64>) -> Vec<f64> {
    pass.samples()
        .filter_map(|s| f(s).map(|n| n as f64))
        .collect()
}

/// The latencies reported per draft: name, value from `begin` in ms, what it is until.
type Latency = (&'static str, fn(&Sample) -> Option<u64>, &'static str);

const LATENCIES: [Latency; 4] = [
    (
        "ttft",
        |s| s.timings.provider_open_ms,
        "until the provider's first stream event (prompt, retrieval and voice included)",
    ),
    (
        "first_inserted_token",
        |s| s.timings.first_token_ms,
        "until the first post-processed text the overlay shows and the user can insert \
         (insertion itself not timed); drafts with no visible text excluded",
    ),
    (
        "first_inserted_char",
        |s| s.first_char_ms,
        "until the stream emitted its first non-whitespace character (the first character the \
         overlay inserts); drafts with no visible text excluded",
    ),
    (
        "total",
        |s| s.timings.total_ms,
        "until the draft stream finished",
    ),
];

/// Prompt token counts per draft: name, value, unit description, direction.
type TokenCount = (
    &'static str,
    fn(&Sample) -> Option<u64>,
    &'static str,
    Direction,
);

const TOKEN_COUNTS: [TokenCount; 3] = [
    (
        "prompt_tokens",
        Sample::prompt_tokens,
        "provider-reported prompt tokens, cached or not",
        Direction::Info,
    ),
    (
        "cached_prompt_tokens",
        Sample::cached_prompt_tokens,
        "prompt tokens the provider served from its prompt cache (llama-server cache_n)",
        Direction::HigherIsBetter,
    ),
    (
        "evaluated_prompt_tokens",
        Sample::evaluated_prompt_tokens,
        "prompt tokens the provider actually read (prompt minus cached; what prefill time \
         grows with)",
        Direction::LowerIsBetter,
    ),
];

fn count(name: String, v: usize, dir: Direction, desc: String) -> Metric {
    Metric::new(name, v as f64, "drafts", dir, desc)
}

/// Metrics of the main pass. `target`: the first-insertable-token p50 target, if any.
pub fn pass_metrics(pass: &Pass, target: Option<f64>) -> Vec<Metric> {
    let p = "doubletap";
    let mut out = Vec::new();
    for (name, f, what) in LATENCIES {
        let Some(s) = summarize(&values(pass, f)) else {
            continue;
        };
        for (q, value) in [("p50", s.p50), ("p95", s.p95)] {
            let mut m = Metric::new(
                format!("{p}.{name}.{q}"),
                value,
                "s",
                Direction::LowerIsBetter,
                format!(
                    "{q} time from Drafter::begin {what}, over {} completed drafts",
                    s.count
                ),
            );
            if name == "first_inserted_token"
                && q == "p50"
                && let Some(t) = target
            {
                m = m.with_target(t);
            }
            out.push(m);
        }
    }
    for (name, f, what) in LATENCIES {
        let Some(s) = summarize(&values(pass, |x| x.since_trigger(f(x)))) else {
            continue;
        };
        for (q, value) in [("p50", s.p50), ("p95", s.p95)] {
            out.push(Metric::new(
                format!("{p}.from_trigger.{name}.{q}"),
                value,
                "s",
                Direction::LowerIsBetter,
                format!(
                    "{q} time from the trigger (the Drafter::prewarm call, so time a prefix \
                     prewarm takes cannot hide) {what}, over {} completed drafts",
                    s.count
                ),
            ));
        }
    }
    for (name, f, what, dir) in TOKEN_COUNTS {
        let Some(s) = summarize(&token_values(pass, f)) else {
            continue;
        };
        for (q, value) in [("p50", s.p50), ("p95", s.p95)] {
            out.push(Metric::new(
                format!("{p}.{name}.{q}"),
                value,
                "tokens",
                dir,
                format!("{q} {what}, over {} drafts with usage", s.count),
            ));
        }
    }
    if let Some(s) = summarize(&values(pass, |s| s.timings.retrieval_ms)) {
        out.push(Metric::new(
            format!("{p}.retrieval.p50"),
            s.p50,
            "s",
            Direction::LowerIsBetter,
            "Median time Drafter::begin waited for retrieval and the voice lookup",
        ));
    }
    out.push(count(
        format!("{p}.requests"),
        pass.outcomes.len(),
        Direction::Info,
        "Timed drafts attempted".into(),
    ));
    out.push(count(
        format!("{p}.completed"),
        pass.completed(),
        Direction::HigherIsBetter,
        "Drafts whose stream finished".into(),
    ));
    out.push(count(
        format!("{p}.failures"),
        pass.failures(),
        Direction::LowerIsBetter,
        "Drafts that failed (begin refused or errored, or the stream errored)".into(),
    ));
    out.push(count(
        format!("{p}.timeouts"),
        pass.timeouts(),
        Direction::LowerIsBetter,
        format!(
            "Drafts that took longer than {} s end to end (cancelled)",
            DRAFT_TIMEOUT.as_secs()
        ),
    ));
    out.push(count(
        format!("{p}.empty_drafts"),
        pass.empty(),
        Direction::LowerIsBetter,
        "Completed drafts with no visible text".into(),
    ));
    out.push(count(
        format!("{p}.retrieval_skipped"),
        pass.samples()
            .filter(|s| s.timings.retrieval_run_ms.is_none())
            .count(),
        Direction::LowerIsBetter,
        "Completed drafts whose retrieval timed out or failed (drafted without context)".into(),
    ));
    // Decode speed: output tokens over the time after the first insertable token.
    let (mut tokens, mut decode_s) = (0u64, 0.0f64);
    for s in pass.samples() {
        if let (Some(n), Some(first), Some(total)) = (
            s.output_tokens,
            s.timings.first_token_ms,
            s.timings.total_ms,
        ) && total > first
        {
            tokens += u64::from(n);
            decode_s += (total - first) as f64 / 1000.0;
        }
    }
    if tokens > 0 && decode_s > 0.0 {
        out.push(Metric::new(
            format!("{p}.output_tokens_per_s"),
            tokens as f64 / decode_s,
            "tokens/s",
            Direction::HigherIsBetter,
            "Provider-reported output tokens over the time from the first insertable token to \
             the end of the stream (drafts with usage only)",
        ));
    }
    let cpu: Vec<f64> = pass.samples().filter_map(|s| s.server_cpu_s).collect();
    if let Some(s) = summarize(&cpu) {
        out.push(Metric::new(
            format!("{p}.server_cpu_s.mean"),
            s.mean,
            "cpu-s",
            Direction::Info,
            format!(
                "Mean CPU seconds the llama-server process spent per draft, trigger to end of \
                 stream ({} drafts)",
                s.count
            ),
        ));
    }
    let prompts: Vec<f64> = pass
        .samples()
        .filter_map(|s| s.input_tokens.map(f64::from))
        .collect();
    if let Some(s) = summarize(&prompts) {
        out.push(Metric::new(
            format!("{p}.prompt_tokens.mean"),
            s.mean,
            "tokens",
            Direction::Info,
            format!(
                "Mean provider-reported prompt tokens ({} drafts with usage)",
                s.count
            ),
        ));
    }
    out
}

/// Metrics of the read-ahead before each draft of the timed pass (`doubletap.read_ahead.`
/// prefix). `min_interval_ms`: the product's `[draft.read_ahead] min_interval_ms`, which caps
/// the projected rate. `skip_target`: give `skipped` a target of 0 (real on-device runs).
pub fn read_ahead_metrics(
    results: &[ReadAheadResult],
    min_interval_ms: u64,
    skip_target: bool,
) -> Vec<Metric> {
    let p = "doubletap.read_ahead";
    let mut out = Vec::new();
    let done: Vec<(u64, &ReadAheadOutcome, Option<f64>)> = results
        .iter()
        .filter_map(|r| match r {
            ReadAheadResult::Done {
                wall_ms,
                outcome,
                server_cpu_s,
            } => Some((*wall_ms, outcome, *server_cpu_s)),
            _ => None,
        })
        .collect();
    let mut reasons: BTreeMap<&'static str, usize> = BTreeMap::new();
    for r in results {
        if let ReadAheadResult::Skipped { reason } = r {
            *reasons.entry(reason).or_default() += 1;
        }
    }
    let timeouts = results
        .iter()
        .filter(|r| matches!(r, ReadAheadResult::TimedOut))
        .count();
    let skipped: usize = reasons.values().sum();
    let counted = |name: &str, v: usize, dir: Direction, desc: String| {
        Metric::new(format!("{p}.{name}"), v as f64, "read-aheads", dir, desc)
    };
    out.push(counted(
        "requests",
        results.len(),
        Direction::Info,
        "Drafter::read_ahead calls, one before each timed draft".into(),
    ));
    out.push(counted(
        "completed",
        done.len(),
        Direction::HigherIsBetter,
        "Read-aheads that prefilled the draft's prompt".into(),
    ));
    let mut m = counted(
        "skipped",
        skipped,
        Direction::LowerIsBetter,
        "Read-aheads that did nothing (any reason; must be 0 with a local provider)".into(),
    );
    if skip_target {
        m = m.with_target(0.0);
    }
    out.push(m);
    for (reason, n) in &reasons {
        out.push(counted(
            &format!("skipped.{reason}"),
            *n,
            Direction::LowerIsBetter,
            format!("Read-aheads skipped as `{reason}`"),
        ));
    }
    out.push(counted(
        "timeouts",
        timeouts,
        Direction::LowerIsBetter,
        "Read-aheads that ran past the per-draft timeout (cancelled)".into(),
    ));
    let wall: Vec<f64> = done.iter().map(|(ms, _, _)| *ms as f64 / 1000.0).collect();
    if let Some(s) = summarize(&wall) {
        for (q, v) in [("p50", s.p50), ("p95", s.p95)] {
            out.push(Metric::new(
                format!("{p}.time.{q}"),
                v,
                "s",
                Direction::LowerIsBetter,
                format!(
                    "{q} wall-clock time of a completed Drafter::read_ahead (how long focus has \
                     to stay for the trigger to find the prompt cached), over {} read-aheads",
                    s.count
                ),
            ));
        }
    }
    for (name, f, what) in [
        (
            "prepare",
            (|o: &ReadAheadOutcome| o.prepare_ms) as fn(&ReadAheadOutcome) -> u64,
            "retrieval and voice lookup until the prompt was built",
        ),
        (
            "warm",
            |o: &ReadAheadOutcome| o.warm_ms,
            "the prefill request (the server reading the prompt)",
        ),
    ] {
        let v: Vec<f64> = done.iter().map(|(_, o, _)| f(o) as f64 / 1000.0).collect();
        if let Some(s) = summarize(&v) {
            out.push(Metric::new(
                format!("{p}.{name}.p50"),
                s.p50,
                "s",
                Direction::LowerIsBetter,
                format!(
                    "Median read-ahead time in {what}, over {} read-aheads",
                    s.count
                ),
            ));
        }
    }
    let cpu: Vec<f64> = done.iter().filter_map(|(_, _, c)| *c).collect();
    if let Some(s) = summarize(&cpu) {
        out.push(Metric::new(
            format!("{p}.server_cpu_s.mean"),
            s.mean,
            "cpu-s",
            Direction::LowerIsBetter,
            format!(
                "Mean CPU seconds (user + system) the llama-server process spent per completed \
                 read-ahead, over {} read-aheads. Battery drain is not metered on CI runners; \
                 CPU-seconds are the input to an energy estimate",
                s.count
            ),
        ));
        out.push(Metric::new(
            format!("{p}.server_cpu_s.p50"),
            s.p50,
            "cpu-s",
            Direction::LowerIsBetter,
            format!(
                "Median CPU seconds the llama-server process spent per completed read-ahead, \
                 over {} read-aheads",
                s.count
            ),
        ));
        for per_min in FOCUS_CHANGES_PER_MIN {
            out.push(Metric::new(
                format!("{p}.core_pct.at_{per_min}_per_min"),
                projected_core_pct(s.mean, per_min, min_interval_ms),
                "%",
                Direction::LowerIsBetter,
                format!(
                    "Projected share of one CPU core spent reading ahead at {per_min} focus \
                     changes per minute, each a new context (mean CPU seconds per read-ahead x \
                     rate, the rate capped at one per {} ms by [draft.read_ahead] \
                     min_interval_ms). CPU time, not measured battery drain (not meterable on CI \
                     runners)",
                    min_interval_ms
                ),
            ));
        }
    }
    out
}

/// `cpu_s` CPU seconds per read-ahead at `per_min` focus changes a minute, as a percentage of
/// one core; the rate is capped at one read-ahead per `min_interval_ms`.
pub fn projected_core_pct(cpu_s: f64, per_min: u32, min_interval_ms: u64) -> f64 {
    let cap = if min_interval_ms == 0 {
        f64::INFINITY
    } else {
        60_000.0 / min_interval_ms as f64
    };
    let rate = f64::from(per_min).min(cap);
    round3(cpu_s * rate / 60.0 * 100.0)
}

/// Metrics of the untimed warm-up draft.
pub fn cold_metrics(cold: &Outcome) -> Vec<Metric> {
    let Outcome::Completed(s) = cold else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, v, what) in [
        (
            "first_inserted_token",
            s.timings.first_token_ms,
            "first insertable token",
        ),
        (
            "first_inserted_char",
            s.first_char_ms,
            "first inserted character",
        ),
        ("ttft", s.timings.provider_open_ms, "provider's first event"),
        ("total", s.timings.total_ms, "end of the stream"),
    ] {
        if let Some(v) = secs(v) {
            out.push(Metric::new(
                format!("doubletap.cold.{name}"),
                v,
                "s",
                Direction::LowerIsBetter,
                format!(
                    "Warm-up draft (first after model start, not in the percentiles): time to \
                     the {what}"
                ),
            ));
        }
    }
    out
}

/// Metrics of the thinking pass (`--local-models`).
pub fn thinking_metrics(pass: &Pass) -> Vec<Metric> {
    let p = "doubletap.thinking_model_default";
    let mut out = Vec::new();
    for (name, v) in [
        (
            "first_inserted_token",
            values(pass, |s| s.timings.first_token_ms),
        ),
        ("first_inserted_char", values(pass, |s| s.first_char_ms)),
        ("ttft", values(pass, |s| s.timings.provider_open_ms)),
        ("total", values(pass, |s| s.timings.total_ms)),
    ] {
        if let Some(s) = summarize(&v) {
            out.push(Metric::new(
                format!("{p}.{name}.p50"),
                s.p50,
                "s",
                Direction::LowerIsBetter,
                format!(
                    "Thinking left at the model's default (Qwen3: thinks first): median {name} \
                     over {} completed drafts",
                    s.count
                ),
            ));
        }
    }
    for (name, v, desc) in [
        ("requests", pass.outcomes.len(), "Drafts attempted"),
        (
            "empty_drafts",
            pass.empty(),
            "Completed drafts with no visible text (the output budget went to thinking)",
        ),
        ("failures", pass.failures(), "Drafts that failed"),
        ("timeouts", pass.timeouts(), "Drafts that timed out"),
    ] {
        let dir = if name == "requests" {
            Direction::Info
        } else {
            Direction::LowerIsBetter
        };
        out.push(count(
            format!("{p}.{name}"),
            v,
            dir,
            format!("Thinking at the model's default: {desc}"),
        ));
    }
    out
}

/// The cache-reuse pass: does the second request read only the new tokens, and what does a
/// finished prefix prewarm save?
#[derive(Debug, Clone, PartialEq)]
pub struct CachePass {
    /// One task drafted twice in a row, `begin` alone both times (the server's cache only).
    pub repeat_first: Outcome,
    pub repeat_second: Outcome,
    /// The same task a third time in the product flow: the trigger's prefix prewarm runs
    /// first, and replaces what the server cached past the prefix.
    pub repeat_after_prewarm: Outcome,
    /// Another task after a cache reset, `begin` alone.
    pub no_prewarm: Outcome,
    /// The same task after another reset, `prewarm` waited for (bounded), then `begin`.
    pub prewarmed: Outcome,
}

/// What each arm of the cache pass is.
const CACHE_ARMS: [(&str, &str); 5] = [
    (
        "repeat_first",
        "cache-reuse pass, first of the same task drafted twice in a row (Drafter::begin alone)",
    ),
    (
        "repeat_second",
        "cache-reuse pass, second of the same task drafted twice in a row (Drafter::begin \
         alone; should read only the tokens that differ from the first)",
    ),
    (
        "repeat_after_prewarm",
        "cache-reuse pass, the same task a third time in the product flow (Drafter::prewarm \
         at the trigger, then begin): what the trigger's prefix prewarm leaves of the cache",
    ),
    (
        "no_prewarm",
        "cache-reuse pass, after a cache reset, Drafter::begin without a prewarm",
    ),
    (
        "prewarmed",
        "cache-reuse pass, same task after another cache reset, Drafter::prewarm finished \
         (waited for, bounded) before Drafter::begin",
    ),
];

/// Metrics of the cache-reuse pass (`doubletap.cache_reuse.` prefix).
pub fn cache_metrics(c: &CachePass) -> Vec<Metric> {
    let p = "doubletap.cache_reuse";
    let mut out = Vec::new();
    let arms = [
        &c.repeat_first,
        &c.repeat_second,
        &c.repeat_after_prewarm,
        &c.no_prewarm,
        &c.prewarmed,
    ];
    for ((arm, what), outcome) in CACHE_ARMS.iter().zip(arms) {
        let Some(s) = outcome.sample() else {
            continue;
        };
        for (name, f, desc, dir) in TOKEN_COUNTS {
            if let Some(v) = f(s) {
                out.push(Metric::new(
                    format!("{p}.{arm}.{name}"),
                    v as f64,
                    "tokens",
                    dir,
                    format!("{what}: {desc}"),
                ));
            }
        }
        for (name, f, until) in LATENCIES {
            if let Some(v) = secs(f(s)) {
                out.push(Metric::new(
                    format!("{p}.{arm}.{name}"),
                    v,
                    "s",
                    Direction::LowerIsBetter,
                    format!("{what}: time from Drafter::begin {until}"),
                ));
            }
        }
        if *arm == "prewarmed" {
            if let Some(v) = secs(s.since_trigger(s.timings.first_token_ms)) {
                out.push(Metric::new(
                    format!("{p}.prewarmed.from_trigger.first_inserted_token"),
                    v,
                    "s",
                    Direction::LowerIsBetter,
                    format!("{what}: time from the trigger, the prewarm wait included"),
                ));
            }
            out.push(Metric::new(
                format!("{p}.prewarmed.prewarm_wait"),
                s.lead_ms as f64 / 1000.0,
                "s",
                Direction::Info,
                format!("{what}: time spent waiting for the prewarm before begin"),
            ));
        }
    }
    if let Some((prompt, evaluated)) = c
        .repeat_second
        .sample()
        .and_then(|s| Some((s.prompt_tokens()?, s.evaluated_prompt_tokens()?)))
        && prompt > 0
    {
        out.push(Metric::new(
            format!("{p}.repeat_second.evaluated_share"),
            round3(evaluated as f64 / prompt as f64),
            "ratio",
            Direction::LowerIsBetter,
            "Share of the second identical request's prompt the provider read again (near 0: \
             only the new tokens were read)",
        ));
    }
    if let (Some(cold), Some(warm)) = (
        c.no_prewarm
            .sample()
            .and_then(Sample::evaluated_prompt_tokens),
        c.prewarmed
            .sample()
            .and_then(Sample::evaluated_prompt_tokens),
    ) {
        out.push(Metric::new(
            format!("{p}.prewarm_saved_tokens"),
            cold as f64 - warm as f64,
            "tokens",
            Direction::HigherIsBetter,
            "Prompt tokens a finished Drafter::prewarm saved the draft (evaluated without a \
             prewarm minus evaluated after one, same task, both after a cache reset)",
        ));
    }
    out
}

/// Fill the provider's prompt cache with an unrelated tiny prompt, so the next request starts
/// from (nearly) nothing cached. Errors are ignored.
async fn reset_cache(provider: &Arc<dyn Provider>, timeout: Duration) {
    let req = ChatRequest::new(vec![Message::user_text("Reply with OK.")])
        .with_system("Benchmark cache reset.")
        .with_max_tokens(1)
        .with_feature("bench");
    let work = async {
        if let Ok(mut stream) = provider.chat(req).await {
            while let Some(ev) = stream.next().await {
                if matches!(ev, Err(_) | Ok(ChatEvent::Done { .. })) {
                    break;
                }
            }
        }
    };
    let _ = tokio::time::timeout(timeout, work).await;
}

/// Run the cache-reuse pass on `repeat` (drafted twice) and `fresh` (reset, no prewarm; reset,
/// waited prewarm).
async fn run_cache_pass(
    drafters: &Drafters,
    provider: &Arc<dyn Provider>,
    repeat: &Task,
    fresh: &Task,
    timeout: Duration,
) -> CachePass {
    let log = |label: &str, task: &Task, o: &Outcome| {
        eprintln!(
            "doubletap cache-reuse {label}: {} -> {}",
            task.id,
            describe(o)
        );
    };
    let repeat_first = draft_task(drafters, repeat, timeout, Flow::NoPrewarm).await;
    log("repeat 1/3", repeat, &repeat_first);
    let repeat_second = draft_task(drafters, repeat, timeout, Flow::NoPrewarm).await;
    log("repeat 2/3", repeat, &repeat_second);
    let repeat_after_prewarm = draft_task(drafters, repeat, timeout, Flow::Product).await;
    log("repeat 3/3 (product flow)", repeat, &repeat_after_prewarm);
    reset_cache(provider, timeout).await;
    let no_prewarm = draft_task(drafters, fresh, timeout, Flow::NoPrewarm).await;
    log("no prewarm", fresh, &no_prewarm);
    reset_cache(provider, timeout).await;
    let prewarmed = draft_task(drafters, fresh, timeout, Flow::WaitForPrewarm(timeout / 2)).await;
    log("prewarmed", fresh, &prewarmed);
    CachePass {
        repeat_first,
        repeat_second,
        repeat_after_prewarm,
        no_prewarm,
        prewarmed,
    }
}

// ---------------------------------------------------------------------------------------------
// The suite

/// The provider for `--provider-url` (the `draft` suite's config logic).
/// `seed` into an OpenAI-compatible config's `extra_body` (llama-server samples with it).
pub(crate) fn with_seed(cfg: &mut ProviderConfig, seed: u64) {
    let mut body = cfg.extra_body.take().unwrap_or_else(|| json!({}));
    if let Some(obj) = body.as_object_mut() {
        obj.insert("seed".into(), json!(seed));
    }
    cfg.extra_body = Some(body);
}

/// A `--provider-url` provider, its label, and the seed it was given (if it takes one).
type UrlProvider = (Arc<dyn Provider>, String, Option<u64>);

fn url_provider(o: &DraftOptions, url: &str, seed: u64) -> Result<UrlProvider, BenchError> {
    let model = o.provider_model.clone().unwrap_or_else(|| "default".into());
    let kind = o
        .provider_kind
        .clone()
        .unwrap_or_else(|| "openai_compatible".into());
    let mut cfg = json!({
        "id": "bench-doubletap",
        "kind": kind,
        "base_url": url,
        "model": model,
    });
    if let Some(var) = &o.api_key_env {
        cfg["api_key"] = json!(format!("env:{var}"));
    }
    let mut cfg: ProviderConfig = serde_json::from_value(cfg)
        .map_err(|e| BenchError::Failed(format!("doubletap provider config: {e}")))?;
    // Only OpenAI-compatible servers take a sampling seed in the body.
    let seeded = (cfg.kind == ProviderKind::OpenaiCompatible).then_some(seed);
    if let Some(seed) = seeded {
        with_seed(&mut cfg, seed);
    }
    let provider = build_provider(&cfg, Arc::new(NullEgress))?;
    Ok((provider, format!("{kind} {url} model {model}"), seeded))
}

/// Where the futures run: a runtime of our own, or the one that owns the local servers.
enum Exec {
    Own(tokio::runtime::Runtime),
    Local(Box<LocalModels>),
}

impl Exec {
    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        match self {
            Self::Own(rt) => rt.block_on(f),
            Self::Local(m) => m.block_on(f),
        }
    }
}

/// What the suite drafts with.
struct Setup {
    exec: Exec,
    provider: Arc<dyn Provider>,
    /// `--local-models` only: the same model with thinking at its default.
    thinking: Option<Arc<dyn Provider>>,
    real: bool,
    desc: String,
    /// The sampling seed sent with every request, where the provider takes one.
    seed: Option<u64>,
    /// `--local-models`: the chat server's process, for CPU metering.
    server_pid: Option<u32>,
}

fn own_runtime() -> Result<tokio::runtime::Runtime, BenchError> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?)
}

fn setup(ctx: &mut RunContext) -> Result<Setup, BenchError> {
    let local = ctx.config.local.clone();
    if local.local_models {
        let models = LocalModels::start(&local, &[RuntimeRole::Chat])?;
        models.record(ctx);
        let seed = ctx.seed();
        // As `LocalModels::chat_with`, plus a fixed sampling seed.
        let seeded = |thinking: Thinking| -> Result<Arc<dyn Provider>, BenchError> {
            let mut cfg = models
                .chat_config(thinking)
                .ok_or_else(|| BenchError::Failed("no local chat model was started".into()))?;
            with_seed(&mut cfg, seed);
            Ok(build_provider_with(
                &cfg,
                Arc::new(NullEgress),
                &models.secrets(),
            )?)
        };
        let provider = seeded(models.thinking)?;
        let thinking = if models.thinking == Thinking::Model {
            // The main pass already thinks at the model's default; no second pass needed.
            None
        } else {
            Some(seeded(Thinking::Model)?)
        };
        let desc = models.chat_label();
        let server_pid = models.server_pid(RuntimeRole::Chat);
        return Ok(Setup {
            exec: Exec::Local(Box::new(models)),
            provider,
            thinking,
            real: true,
            desc,
            seed: Some(seed),
            server_pid,
        });
    }
    let opts = ctx.config.draft.clone();
    if let Some(url) = &opts.provider_url {
        let (provider, desc, seed) = url_provider(&opts, url, ctx.seed())?;
        return Ok(Setup {
            exec: Exec::Own(own_runtime()?),
            provider,
            thinking: None,
            real: true,
            desc,
            seed,
            server_pid: None,
        });
    }
    let locality = match opts.draft_locality {
        DraftLocality::Cloud => Locality::Cloud,
        DraftLocality::Local => Locality::OnDevice,
    };
    let latency = FakeLatency {
        ttft_ms: opts.fake_ttft_ms,
        prefill_ms_per_1k: opts.fake_prefill_ms_per_1k,
        token_ms: opts.fake_token_ms,
    };
    ctx.param("fake.ttft_ms", latency.ttft_ms);
    ctx.param("fake.prefill_ms_per_1k", latency.prefill_ms_per_1k);
    ctx.param("fake.token_ms", latency.token_ms);
    Ok(Setup {
        exec: Exec::Own(own_runtime()?),
        provider: Arc::new(FakeChat::new(locality, latency)),
        thinking: None,
        real: false,
        desc: "fake-deterministic (in-process)".into(),
        seed: None,
        server_pid: None,
    })
}

/// `n` indices out of `0..all`, evenly spread (all of them when `n >= all`).
pub(crate) fn spread(all: usize, n: usize) -> Vec<usize> {
    if n >= all {
        return (0..all).collect();
    }
    (0..n).map(|i| i * all / n).collect()
}

/// Up to `n` task indices not in `used` (from the end, so they differ from the spread), else
/// the first of `used`.
fn unused(all: usize, used: &[usize], n: usize) -> Vec<usize> {
    let mut out: Vec<usize> = (0..all)
        .rev()
        .filter(|i| !used.contains(i))
        .take(n)
        .collect();
    if out.len() < n {
        out.extend(used.iter().copied());
        out.truncate(n);
    }
    out
}

/// Run the suite with a provider (tests inject their own).
fn run_with(ctx: &mut RunContext, s: Setup, timeout: Duration) -> Result<Vec<Metric>, BenchError> {
    let corpus = Corpus::load().map_err(BenchError::Failed)?;
    // The tasks before the draft suite's artefact tasks, so runs stay comparable as it grows.
    let all = corpus.tasks.len().min(super::draft::SPREAD_TASKS);
    let n = if s.real {
        ctx.scale().pick(4, 12, 24, all)
    } else {
        ctx.scale().pick(12, 33, all, all)
    };
    let tasks = spread(all, n);
    let warmup = unused(all, &tasks, 1);
    let locality = s.provider.meta().locality;
    let target = s.real.then(|| latency_target(locality));
    let host = HostInfo::detect();
    let local_budget = ctx.config.draft.draft_local_budget;

    let mut config = DraftConfig::default();
    config.local.enabled = local_budget.is_on();
    config.grounding = ctx.config.doubletap.doubletap_grounding.is_on();
    config.grounding_check = super::draft::grounding_check(&ctx.config.draft);
    if s.real {
        // Greedy sampling, so before/after runs differ only by what changed.
        config.temperature = Some(0.0);
    }
    let local_applies = config.local.enabled && locality != Locality::Cloud;
    let read_ahead = ctx.config.doubletap.doubletap_read_ahead;
    let product_interval_ms = ReadAheadConfig::default().min_interval_ms;
    if read_ahead.is_on() {
        // Every task stands for a focus change well apart from the last; the product's rate
        // limit is applied to the CPU projection instead.
        config.read_ahead.enabled = true;
        config.read_ahead.min_interval_ms = 0;
    }
    let probe = Probe {
        read_ahead: read_ahead.is_on(),
        server_pid: s.server_pid,
    };

    ctx.param("model", s.desc.clone());
    ctx.param("model.locality", format!("{locality:?}"));
    ctx.param(
        "engine",
        "nugget_draft::Drafter (DraftConfig::default, local budget and temperature as below)",
    );
    ctx.param("tasks", tasks.len());
    ctx.param("tasks.available", all);
    ctx.param(
        "warmup_task",
        warmup
            .first()
            .map(|&i| corpus.tasks[i].id.clone())
            .unwrap_or_default(),
    );
    ctx.param("timeout_s", timeout.as_secs_f64());
    ctx.param("draft_local_budget", local_budget.as_str());
    ctx.param(
        "grounding",
        ctx.config.doubletap.doubletap_grounding.as_str(),
    );
    ctx.param(
        "grounding_check",
        super::draft::check_name(config.grounding_check),
    );
    ctx.param("draft_local_budget.applies", local_applies);
    ctx.param(
        "prompt_budget_tokens",
        if local_applies {
            config.local.budget_tokens
        } else {
            config.budget_tokens
        },
    );
    if local_applies {
        ctx.param("local_budget.max_snippets", config.local.max_snippets);
    }
    ctx.param(
        "temperature",
        config
            .temperature
            .map_or(serde_json::Value::Null, |t| json!(t)),
    );
    ctx.param("seed", s.seed.map_or(serde_json::Value::Null, |v| json!(v)));
    ctx.param("retrieval", RETRIEVAL_DESC);
    ctx.param("prewarm", PREWARM_DESC);
    ctx.param("voice", VOICE_DESC);
    ctx.param(
        "timed_from",
        "Drafter::begin (a11y read and IPC not included); from_trigger.* from the \
         Drafter::prewarm call",
    );
    ctx.param("read_ahead", read_ahead.as_str());
    if read_ahead.is_on() {
        ctx.param(
            "read_ahead.flow",
            "per task: Drafter::read_ahead with the trigger's DraftRequest, waited for (bounded \
             by the per-draft timeout), then the trigger flow unchanged; from_trigger.* starts \
             after the read-ahead",
        );
        ctx.param(
            "read_ahead.min_interval_ms",
            config.read_ahead.min_interval_ms,
        );
        ctx.param("read_ahead.product_min_interval_ms", product_interval_ms);
    }
    ctx.param(
        "server_cpu",
        match s.server_pid {
            Some(pid) => format!(
                "llama-server pid {pid}: user + system CPU time from {}",
                if cfg!(target_os = "linux") {
                    "/proc/<pid>/stat"
                } else if cfg!(target_os = "macos") {
                    "ps -o cputime"
                } else {
                    "nowhere (not metered on this OS)"
                }
            ),
            None => "not metered (no local server process)".to_owned(),
        },
    );
    ctx.param("meets_16gb", host.meets_16gb);
    if !matches!(s.exec, Exec::Local(_)) {
        ctx.param("hardware", host.to_json());
    }
    ctx.param(
        "target.first_inserted_token.p50_s",
        target.map_or(serde_json::Value::Null, serde_json::Value::from),
    );

    let stores = build_stores(&corpus)?;
    let drafters = build_drafters(&corpus, &stores, s.provider.clone(), &config);
    let started = Instant::now();
    let (idle, idle_s, cold, pass) = s.exec.block_on(async {
        let t = Instant::now();
        let idle = drafters.prewarm_idle(timeout).await;
        let idle_s = t.elapsed().as_secs_f64();
        let cold = run_pass(&drafters, &corpus, &warmup, timeout, "warm-up", probe).await;
        let pass = run_pass(&drafters, &corpus, &tasks, timeout, "timed", probe).await;
        (idle, idle_s, cold, pass)
    });
    ctx.param("main_pass_s", round3(started.elapsed().as_secs_f64()));
    ctx.param(
        "idle_prewarm",
        match &idle {
            None => "none (Drafter::prewarm_idle warmed nothing)".to_owned(),
            Some((id, true)) => format!("{id}: finished before the warm-up draft"),
            Some((id, false)) => format!("{id}: still running after {} s", timeout.as_secs()),
        },
    );
    ctx.param("idle_prewarm_s", round3(idle_s));
    if let Some(Outcome::Failed(e)) = cold.outcomes.first() {
        ctx.param("cold.error", e.clone());
    }
    if let Some(Outcome::TimedOut) = cold.outcomes.first() {
        ctx.param("cold.error", "timed out");
    }
    if pass.completed() == 0 {
        return Err(BenchError::Failed(format!(
            "no draft completed: {} failed, {} timed out (first error: {})",
            pass.failures(),
            pass.timeouts(),
            pass.first_error().unwrap_or("none")
        )));
    }
    if let Some(e) = pass.first_error() {
        ctx.param("first_error", e.to_owned());
    }

    let mut out = pass_metrics(&pass, target);
    if read_ahead.is_on() {
        let on_device = s.real && locality == Locality::OnDevice;
        out.extend(read_ahead_metrics(
            &pass.read_aheads,
            product_interval_ms,
            on_device,
        ));
    }
    if let Some(c) = cold.outcomes.first() {
        out.extend(cold_metrics(c));
    }

    // Tasks the main pass did not draft, so no prompt-cache reuse flatters the later passes.
    let main = [tasks.as_slice(), &warmup].concat();
    let thinking_tasks = unused(all, &main, THINKING_TASKS);
    let cache_tasks = unused(all, &[main.as_slice(), &thinking_tasks].concat(), 2);
    let (repeat, fresh) = match cache_tasks.as_slice() {
        [a, b, ..] => (*a, *b),
        [a] => (*a, *a),
        [] => (tasks[0], tasks[0]),
    };
    ctx.param(
        "cache_reuse",
        format!(
            "{} drafted twice in a row (begin alone), then a third time in the product flow; {} after a cache reset without a \
             prewarm, then after another reset with Drafter::prewarm waited for (up to {} s)",
            corpus.tasks[repeat].id,
            corpus.tasks[fresh].id,
            (timeout / 2).as_secs_f64()
        ),
    );
    let prefix_before = drafters.warmer.prefix_calls();
    let cache = s.exec.block_on(run_cache_pass(
        &drafters,
        &s.provider,
        &corpus.tasks[repeat],
        &corpus.tasks[fresh],
        timeout,
    ));
    ctx.param("prefix_prewarm_calls", drafters.warmer.prefix_calls());
    ctx.param(
        "cache_reuse.prefix_prewarm_calls",
        drafters.warmer.prefix_calls() - prefix_before,
    );
    if let Some(settled) = cache.prewarmed.sample().and_then(|s| s.prewarm_settled) {
        ctx.param("cache_reuse.prewarm_finished", settled);
    }
    let errors: Vec<String> = [
        &cache.repeat_first,
        &cache.repeat_second,
        &cache.repeat_after_prewarm,
        &cache.no_prewarm,
        &cache.prewarmed,
    ]
    .iter()
    .filter_map(|o| match o {
        Outcome::Failed(e) => Some(e.clone()),
        Outcome::TimedOut => Some("timed out".into()),
        Outcome::Completed(_) => None,
    })
    .collect();
    if !errors.is_empty() {
        ctx.param("cache_reuse.errors", errors);
    }
    out.extend(cache_metrics(&cache));

    match &s.thinking {
        None => ctx.param(
            "thinking_pass",
            if matches!(s.exec, Exec::Local(_)) {
                "skipped (--local-thinking model: the main pass already thinks)"
            } else {
                "skipped (not --local-models)"
            },
        ),
        Some(provider) => {
            ctx.param(
                "thinking_pass",
                format!(
                    "{} drafts, thinking at the model's default",
                    thinking_tasks.len()
                ),
            );
            ctx.param(
                "thinking_tasks",
                thinking_tasks
                    .iter()
                    .map(|&i| corpus.tasks[i].id.clone())
                    .collect::<Vec<_>>(),
            );
            let drafters = build_drafters(&corpus, &stores, provider.clone(), &config);
            let thinking = s.exec.block_on(run_pass(
                &drafters,
                &corpus,
                &thinking_tasks,
                timeout,
                "thinking",
                Probe {
                    read_ahead: false,
                    ..probe
                },
            ));
            out.extend(thinking_metrics(&thinking));
        }
    }
    Ok(out)
}

pub struct DoubleTapBench;

impl Benchmark for DoubleTapBench {
    fn name(&self) -> &'static str {
        "doubletap"
    }

    fn description(&self) -> &'static str {
        "Double Tap latency through the real drafting engine: p50/p95 time to the first \
         insertable token, provider first token and full draft"
    }

    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        let s = setup(ctx)?;
        run_with(ctx, s, DRAFT_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{RunConfig, Scale, run_benchmark};
    use crate::suites::draft::Switch;
    use crate::{Report, Status};

    fn value(r: &Report, name: &str) -> f64 {
        r.metric(name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .value
    }

    /// Run with an injected provider through the normal report path.
    struct With {
        provider: Arc<dyn Provider>,
        thinking: Option<Arc<dyn Provider>>,
        real: bool,
        timeout: Duration,
    }

    impl Benchmark for With {
        fn name(&self) -> &'static str {
            "doubletap"
        }
        fn description(&self) -> &'static str {
            "test"
        }
        fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
            let s = Setup {
                exec: Exec::Own(own_runtime()?),
                provider: self.provider.clone(),
                thinking: self.thinking.clone(),
                real: self.real,
                desc: "test".into(),
                seed: None,
                server_pid: None,
            };
            run_with(ctx, s, self.timeout)
        }
    }

    fn smoke() -> RunConfig {
        RunConfig {
            scale: Scale::Smoke,
            ..RunConfig::default()
        }
    }

    #[test]
    fn fake_smoke_run_reports_every_latency_metric() {
        let r = run_benchmark(&DoubleTapBench, &smoke());
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        for name in [
            "doubletap.ttft.p50",
            "doubletap.ttft.p95",
            "doubletap.first_inserted_token.p50",
            "doubletap.first_inserted_token.p95",
            "doubletap.total.p50",
            "doubletap.total.p95",
            "doubletap.retrieval.p50",
            "doubletap.output_tokens_per_s",
            "doubletap.prompt_tokens.mean",
            "doubletap.cold.first_inserted_token",
            "doubletap.cold.first_inserted_char",
            "doubletap.first_inserted_char.p50",
            "doubletap.first_inserted_char.p95",
            "doubletap.from_trigger.ttft.p50",
            "doubletap.from_trigger.first_inserted_token.p50",
            "doubletap.from_trigger.first_inserted_char.p95",
            "doubletap.from_trigger.total.p95",
            "doubletap.prompt_tokens.p50",
            "doubletap.prompt_tokens.p95",
            "doubletap.cached_prompt_tokens.p50",
            "doubletap.evaluated_prompt_tokens.p50",
            "doubletap.evaluated_prompt_tokens.p95",
            "doubletap.cache_reuse.repeat_first.evaluated_prompt_tokens",
            "doubletap.cache_reuse.repeat_second.prompt_tokens",
            "doubletap.cache_reuse.repeat_second.cached_prompt_tokens",
            "doubletap.cache_reuse.repeat_second.evaluated_prompt_tokens",
            "doubletap.cache_reuse.repeat_second.first_inserted_token",
            "doubletap.cache_reuse.repeat_second.first_inserted_char",
            "doubletap.cache_reuse.repeat_second.evaluated_share",
            "doubletap.cache_reuse.no_prewarm.evaluated_prompt_tokens",
            "doubletap.cache_reuse.prewarmed.evaluated_prompt_tokens",
            "doubletap.cache_reuse.prewarmed.prewarm_wait",
            "doubletap.cache_reuse.prewarmed.from_trigger.first_inserted_token",
            "doubletap.cache_reuse.prewarm_saved_tokens",
        ] {
            assert!(r.metric(name).is_some(), "missing {name}");
        }
        // Inserted text starts with a character, so it never comes before the first token.
        assert!(
            value(&r, "doubletap.first_inserted_char.p50")
                >= value(&r, "doubletap.first_inserted_token.p50")
        );
        // The second of two identical requests reads (almost) nothing again.
        assert!(
            value(&r, "doubletap.cache_reuse.repeat_second.evaluated_share") < 0.05,
            "{}",
            value(&r, "doubletap.cache_reuse.repeat_second.evaluated_share")
        );
        assert_eq!(
            value(
                &r,
                "doubletap.cache_reuse.repeat_second.cached_prompt_tokens"
            ) + value(
                &r,
                "doubletap.cache_reuse.repeat_second.evaluated_prompt_tokens"
            ),
            value(&r, "doubletap.cache_reuse.repeat_second.prompt_tokens")
        );
        assert_eq!(r.config.params["draft_local_budget"], "on");
        assert!(r.config.params["temperature"].is_null());
        assert!(r.config.params["seed"].is_null());
        assert!(r.config.params["idle_prewarm"].is_string());
        assert_eq!(value(&r, "doubletap.requests"), 12.0);
        assert_eq!(value(&r, "doubletap.completed"), 12.0);
        assert_eq!(value(&r, "doubletap.failures"), 0.0);
        assert_eq!(value(&r, "doubletap.timeouts"), 0.0);
        assert_eq!(value(&r, "doubletap.empty_drafts"), 0.0);
        assert_eq!(value(&r, "doubletap.retrieval_skipped"), 0.0);
        // The prompt carries retrieved context and voice exemplars, not just the thread.
        assert!(value(&r, "doubletap.prompt_tokens.mean") > 200.0);
        // Fake numbers are not product numbers: no target.
        assert!(r.targets.is_empty());
        assert_eq!(r.config.params["tasks"], 12);
        assert_eq!(
            r.config.params["thinking_pass"],
            "skipped (not --local-models)"
        );
        assert!(
            r.metrics
                .iter()
                .all(|m| !m.name.contains("thinking_model_default"))
        );
        assert!(r.config.params["meets_16gb"].is_boolean());
    }

    #[test]
    fn fake_latency_shows_in_the_percentiles() {
        let mut cfg = smoke();
        cfg.draft.fake_ttft_ms = 40;
        cfg.draft.fake_token_ms = 2;
        let r = run_benchmark(&DoubleTapBench, &cfg);
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        let first = value(&r, "doubletap.first_inserted_token.p50");
        assert!(first >= 0.04, "{first}");
        assert!(value(&r, "doubletap.ttft.p50") >= 0.04);
        assert!(value(&r, "doubletap.total.p50") > first);
    }

    #[test]
    fn real_providers_get_the_target_for_their_locality() {
        assert_eq!(latency_target(Locality::OnDevice), TTFT_TARGET_LOCAL_S);
        assert_eq!(latency_target(Locality::Cloud), TTFT_TARGET_CLOUD_S);
        assert_eq!(latency_target(Locality::LocalNetwork), TTFT_TARGET_CLOUD_S);
        let r = run_benchmark(
            &With {
                provider: Arc::new(FakeChat::new(Locality::OnDevice, FakeLatency::default())),
                thinking: None,
                real: true,
                timeout: Duration::from_secs(30),
            },
            &smoke(),
        );
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert_eq!(r.targets.len(), 1);
        assert_eq!(r.targets[0].metric, "doubletap.first_inserted_token.p50");
        assert_eq!(r.targets[0].target, TTFT_TARGET_LOCAL_S);
        assert!(r.targets[0].pass);
        // Real-provider task count at smoke scale.
        assert_eq!(value(&r, "doubletap.requests"), 4.0);

        let slow = pass_metrics(
            &Pass {
                outcomes: vec![Outcome::Completed(Sample {
                    timings: DraftTimings {
                        first_token_ms: Some(2_000),
                        provider_open_ms: Some(1_900),
                        total_ms: Some(4_000),
                        ..DraftTimings::default()
                    },
                    text_chars: 10,
                    ..Sample::default()
                })],
                ..Pass::default()
            },
            Some(latency_target(Locality::Cloud)),
        );
        let m = slow
            .iter()
            .find(|m| m.name == "doubletap.first_inserted_token.p50")
            .unwrap();
        assert_eq!(m.target_met, Some(false));
    }

    #[test]
    fn failures_are_counted_not_hidden() {
        // Every third request fails before streaming (the warm-up is request 1).
        let r = run_benchmark(
            &With {
                provider: Arc::new(
                    FakeChat::new(Locality::Cloud, FakeLatency::default()).failing_every(3),
                ),
                thinking: None,
                real: false,
                timeout: Duration::from_secs(30),
            },
            &smoke(),
        );
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        // Requests 3, 6, 9, 12 fail: timed drafts 2, 5, 8, 11.
        assert_eq!(value(&r, "doubletap.failures"), 4.0);
        assert_eq!(value(&r, "doubletap.completed"), 8.0);
        assert_eq!(value(&r, "doubletap.requests"), 12.0);
        assert!(
            r.config.params["first_error"]
                .as_str()
                .unwrap()
                .contains("injected failure")
        );
    }

    #[test]
    fn a_stalled_provider_times_out_and_fails_the_suite() {
        let r = run_benchmark(
            &With {
                provider: Arc::new(
                    FakeChat::new(Locality::OnDevice, FakeLatency::default()).stalling(),
                ),
                thinking: None,
                real: true,
                timeout: Duration::from_millis(50),
            },
            &smoke(),
        );
        assert_eq!(r.status, Status::Failed);
        let msg = r.message.unwrap();
        assert!(msg.contains("no draft completed"), "{msg}");
        assert!(msg.contains("4 timed out"), "{msg}");
        assert_eq!(r.config.params["cold.error"], "timed out");
    }

    #[test]
    fn thinking_pass_runs_only_with_a_thinking_provider() {
        let r = run_benchmark(
            &With {
                provider: Arc::new(FakeChat::new(Locality::OnDevice, FakeLatency::default())),
                thinking: Some(Arc::new(
                    FakeChat::new(Locality::OnDevice, FakeLatency::default()).stalling(),
                )),
                real: true,
                timeout: Duration::from_millis(500),
            },
            &smoke(),
        );
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert_eq!(
            value(&r, "doubletap.thinking_model_default.requests"),
            THINKING_TASKS as f64
        );
        assert_eq!(
            value(&r, "doubletap.thinking_model_default.timeouts"),
            THINKING_TASKS as f64
        );
        assert!(
            r.metric("doubletap.thinking_model_default.first_inserted_token.p50")
                .is_none()
        );
        let used: Vec<String> =
            serde_json::from_value(r.config.params["thinking_tasks"].clone()).unwrap();
        assert_eq!(used.len(), THINKING_TASKS);
    }

    #[test]
    fn mocked_openai_compatible_server_through_provider_url() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(MockServer::start());
        let body: String = [
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"Sounds good, "}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"see you Friday."}}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":812,"completion_tokens":7}}"#,
            "[DONE]",
        ]
        .iter()
        .map(|d| format!("data: {d}\n\n"))
        .collect();
        rt.block_on(
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/event-stream")
                        .set_body_string(body),
                )
                .mount(&server),
        );
        let mut cfg = smoke();
        cfg.draft.provider_url = Some(format!("{}/v1", server.uri()));
        cfg.draft.provider_model = Some("qwen3-4b".into());
        let r = run_benchmark(&DoubleTapBench, &cfg);
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert_eq!(value(&r, "doubletap.completed"), 4.0);
        assert_eq!(value(&r, "doubletap.prompt_tokens.mean"), 812.0);
        assert_eq!(value(&r, "doubletap.prompt_tokens.p50"), 812.0);
        // No cache detail in the usage: nothing evaluated is claimed.
        assert!(r.metric("doubletap.evaluated_prompt_tokens.p50").is_none());
        assert!(r.metric("doubletap.first_inserted_char.p50").is_some());
        // Determinism: greedy sampling and a fixed seed for a real provider.
        assert_eq!(r.config.params["temperature"], 0.0);
        assert_eq!(r.config.params["seed"], cfg.seed);
        assert_eq!(value(&r, "doubletap.empty_drafts"), 0.0);
        // Loopback is on-device: the local target applies.
        assert_eq!(r.config.params["model.locality"], "OnDevice");
        assert_eq!(r.targets[0].target, TTFT_TARGET_LOCAL_S);
        let sent = rt.block_on(server.received_requests()).unwrap();
        // Streamed requests only (a prefix prewarm is a non-streamed chat completion).
        let streamed: Vec<serde_json::Value> = sent
            .iter()
            .filter(|q| q.url.path().ends_with("/chat/completions"))
            .filter_map(|q| serde_json::from_slice::<serde_json::Value>(&q.body).ok())
            .filter(|b| b["stream"] == true)
            .collect();
        assert_eq!(
            streamed.len(),
            12,
            "warm-up + 4 timed drafts + cache pass (3 repeats, 2 resets, 2 drafts)"
        );
        for b in &streamed {
            assert_eq!(b["seed"], cfg.seed, "{b}");
        }
        let drafts: Vec<_> = streamed.iter().filter(|b| b["max_tokens"] != 1).collect();
        assert_eq!(drafts.len(), 10);
        assert!(drafts.iter().all(|b| b["temperature"] == 0.0), "{drafts:?}");
    }

    fn chat_req(system: &str, user: &str) -> ChatRequest {
        ChatRequest::new(vec![Message::user_text(user)]).with_system(system)
    }

    async fn usage(p: &FakeChat, req: ChatRequest) -> (u32, u32) {
        let mut stream = p.chat(req).await.unwrap();
        while let Some(ev) = stream.next().await {
            if let Ok(ChatEvent::Stats(s)) = ev {
                return (s.input_tokens.unwrap(), s.cached_input_tokens.unwrap());
            }
        }
        panic!("no stats");
    }

    #[test]
    fn fake_provider_simulates_a_prefix_cache() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let p = FakeChat::new(Locality::OnDevice, FakeLatency::default());
            let rules = "Draft a reply in the user's voice. ".repeat(40);
            let (input, cached) = usage(&p, chat_req(&rules, "Can we meet Friday?")).await;
            assert_eq!(cached, 0);
            assert!(input > 200, "{input}");
            // The same request again: all of it from the cache.
            let (again, cached) = usage(&p, chat_req(&rules, "Can we meet Friday?")).await;
            assert_eq!((again, cached), (input, input));
            // A new question after the same rules: the rules come from the cache.
            let (input, cached) = usage(&p, chat_req(&rules, "Lunch on Monday instead?")).await;
            let rules_tokens = u32::try_from(tokens(&rules)).unwrap();
            assert!(
                cached >= rules_tokens - 2 && cached < input,
                "{cached} {input}"
            );
            // A prefix prewarm fills the cache ahead of the request.
            let other = "Answer briefly and politely. ".repeat(40);
            p.prewarm_prefix(&chat_req(&other, "ignored"))
                .await
                .unwrap();
            let (_, cached) = usage(&p, chat_req(&other, "Is the doc ready?")).await;
            let other_tokens = u32::try_from(tokens(&other)).unwrap();
            assert!(cached >= other_tokens - 2, "{cached} < {other_tokens}");
        });
    }

    #[test]
    fn fake_prefill_is_paid_only_for_uncached_tokens() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let p = FakeChat::new(
                Locality::OnDevice,
                FakeLatency {
                    prefill_ms_per_1k: 1_000,
                    ..FakeLatency::default()
                },
            );
            let rules = "Draft a reply in the user's voice. ".repeat(60);
            let first_event = |req| {
                let p = &p;
                async move {
                    let t = Instant::now();
                    let mut stream = p.chat(req).await.unwrap();
                    stream.next().await;
                    t.elapsed()
                }
            };
            let cold = first_event(chat_req(&rules, "Friday?")).await;
            let warm = first_event(chat_req(&rules, "Friday?")).await;
            assert!(cold >= Duration::from_millis(200), "{cold:?}");
            assert!(warm < cold / 4, "{warm:?} vs {cold:?}");
        });
    }

    #[test]
    fn a_finished_prefix_prewarm_is_measured() {
        let cfg = RunConfig {
            draft: DraftOptions {
                draft_locality: DraftLocality::Local,
                fake_prefill_ms_per_1k: 20,
                ..DraftOptions::default()
            },
            ..smoke()
        };
        let r = run_benchmark(&DoubleTapBench, &cfg);
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert!(!r.config.params.contains_key("cache_reuse.errors"));
        let saved = value(&r, "doubletap.cache_reuse.prewarm_saved_tokens");
        let calls = r.config.params["cache_reuse.prefix_prewarm_calls"]
            .as_u64()
            .unwrap();
        if calls > 0 {
            // The engine handed a stable prefix over: the prewarmed draft read less.
            assert!(saved > 0.0, "{saved}");
            assert_eq!(r.config.params["cache_reuse.prewarm_finished"], true);
        } else {
            // Connection prewarm only: nothing is cached ahead of the draft.
            assert!(saved.abs() < 1.0, "{saved}");
        }
        // Time from the trigger includes the prewarm wait.
        assert!(
            value(
                &r,
                "doubletap.cache_reuse.prewarmed.from_trigger.first_inserted_token"
            ) >= value(&r, "doubletap.cache_reuse.prewarmed.prewarm_wait")
        );
    }

    #[test]
    fn local_budget_flag_is_recorded_and_never_grows_the_prompt() {
        let run = |budget| {
            let cfg = RunConfig {
                draft: DraftOptions {
                    draft_locality: DraftLocality::Local,
                    draft_local_budget: budget,
                    ..DraftOptions::default()
                },
                ..smoke()
            };
            run_benchmark(&DoubleTapBench, &cfg)
        };
        let on = run(Switch::On);
        let off = run(Switch::Off);
        assert_eq!(on.status, Status::Ok, "{:?}", on.message);
        assert_eq!(off.status, Status::Ok, "{:?}", off.message);
        assert_eq!(on.config.params["draft_local_budget"], "on");
        assert_eq!(on.config.params["draft_local_budget.applies"], true);
        assert_eq!(off.config.params["draft_local_budget"], "off");
        assert_eq!(off.config.params["draft_local_budget.applies"], false);
        assert!(
            on.config.params["prompt_budget_tokens"].as_u64()
                < off.config.params["prompt_budget_tokens"].as_u64()
        );
        let tokens = |r: &Report| value(r, "doubletap.prompt_tokens.p50");
        assert!(
            tokens(&on) <= tokens(&off),
            "{} > {}",
            tokens(&on),
            tokens(&off)
        );
    }

    fn read_ahead_run(read_ahead: Switch) -> Report {
        let cfg = RunConfig {
            draft: DraftOptions {
                draft_locality: DraftLocality::Local,
                fake_prefill_ms_per_1k: 20,
                ..DraftOptions::default()
            },
            doubletap: DoubletapOptions {
                doubletap_read_ahead: read_ahead,
                ..DoubletapOptions::default()
            },
            ..smoke()
        };
        run_benchmark(&DoubleTapBench, &cfg)
    }

    #[test]
    fn read_ahead_is_off_by_default() {
        assert_eq!(
            DoubletapOptions::default().doubletap_read_ahead,
            Switch::Off
        );
        let r = run_benchmark(&DoubleTapBench, &smoke());
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert_eq!(r.config.params["read_ahead"], "off");
        assert!(
            r.metrics
                .iter()
                .all(|m| !m.name.starts_with("doubletap.read_ahead."))
        );
        // The fake runs in process: no server CPU to meter.
        assert!(r.metric("doubletap.server_cpu_s.mean").is_none());
        assert_eq!(
            r.config.params["server_cpu"],
            "not metered (no local server process)"
        );
    }

    #[test]
    fn read_ahead_runs_before_every_trigger() {
        let off = read_ahead_run(Switch::Off);
        let on = read_ahead_run(Switch::On);
        assert_eq!(on.status, Status::Ok, "{:?}", on.message);
        assert_eq!(on.config.params["read_ahead"], "on");
        assert_eq!(on.config.params["read_ahead.min_interval_ms"], 0);
        assert_eq!(
            on.config.params["read_ahead.product_min_interval_ms"],
            ReadAheadConfig::default().min_interval_ms
        );
        let requests = value(&on, "doubletap.read_ahead.requests");
        assert_eq!(requests, value(&on, "doubletap.requests"));
        let completed = value(&on, "doubletap.read_ahead.completed");
        let skipped = value(&on, "doubletap.read_ahead.skipped");
        assert_eq!(
            completed + skipped + value(&on, "doubletap.read_ahead.timeouts"),
            requests
        );
        // The fake is not a real provider: no target on the skip count.
        assert!(on.targets.is_empty());
        // The trigger flow itself is unchanged.
        assert_eq!(value(&on, "doubletap.completed"), 12.0);
        if completed == 0.0 {
            // The engine reads nothing ahead (read-ahead not implemented or turned off).
            assert!(on.metric("doubletap.read_ahead.time.p50").is_none());
            return;
        }
        // A local provider with read-ahead enabled never skips.
        assert_eq!(skipped, 0.0);
        for name in [
            "doubletap.read_ahead.time.p50",
            "doubletap.read_ahead.time.p95",
            "doubletap.read_ahead.prepare.p50",
            "doubletap.read_ahead.warm.p50",
        ] {
            assert!(on.metric(name).is_some(), "missing {name}");
        }
        // What was read ahead is served from the cache at the trigger.
        let evaluated = |r: &Report| value(r, "doubletap.evaluated_prompt_tokens.p50");
        let cached = |r: &Report| value(r, "doubletap.cached_prompt_tokens.p50");
        assert!(
            evaluated(&on) < evaluated(&off),
            "{} >= {}",
            evaluated(&on),
            evaluated(&off)
        );
        assert!(
            cached(&on) > cached(&off),
            "{} <= {}",
            cached(&on),
            cached(&off)
        );
    }

    #[test]
    fn read_ahead_metrics_count_skips_and_project_cpu() {
        let done = |wall_ms, cpu| ReadAheadResult::Done {
            wall_ms,
            outcome: ReadAheadOutcome {
                provider: ProviderId::new("local-chat"),
                prompt_chars: 3_000,
                prepare_ms: 10,
                warm_ms: wall_ms - 10,
            },
            server_cpu_s: Some(cpu),
        };
        let results = vec![
            done(2_000, 3.0),
            done(4_000, 5.0),
            ReadAheadResult::Skipped {
                reason: "rate_limited",
            },
            ReadAheadResult::TimedOut,
        ];
        let m = read_ahead_metrics(&results, 5_000, true);
        let get = |name: &str| {
            m.iter()
                .find(|x| x.name == name)
                .unwrap_or_else(|| panic!("missing {name}"))
        };
        assert_eq!(get("doubletap.read_ahead.requests").value, 4.0);
        assert_eq!(get("doubletap.read_ahead.completed").value, 2.0);
        assert_eq!(get("doubletap.read_ahead.skipped").value, 1.0);
        assert_eq!(get("doubletap.read_ahead.skipped").target_met, Some(false));
        assert_eq!(get("doubletap.read_ahead.skipped.rate_limited").value, 1.0);
        assert_eq!(get("doubletap.read_ahead.timeouts").value, 1.0);
        assert_eq!(get("doubletap.read_ahead.server_cpu_s.mean").value, 4.0);
        // 4 CPU-s per read-ahead at 6 a minute: 24 CPU-s a minute, 40% of a core.
        assert_eq!(
            get("doubletap.read_ahead.core_pct.at_6_per_min").value,
            40.0
        );
        assert_eq!(
            get("doubletap.read_ahead.core_pct.at_1_per_min").value,
            round3(4.0 / 60.0 * 100.0)
        );
        assert!(
            get("doubletap.read_ahead.server_cpu_s.mean")
                .description
                .contains("Battery")
        );
        // Without CPU readings (in-process fake) nothing is projected.
        let m = read_ahead_metrics(
            &[ReadAheadResult::Skipped { reason: "disabled" }],
            5_000,
            false,
        );
        assert!(m.iter().all(|x| !x.name.contains("core_pct")));
        assert!(m.iter().all(|x| x.target.is_none()));
    }

    #[test]
    fn projected_cpu_is_capped_by_the_rate_limit() {
        assert_eq!(projected_core_pct(6.0, 1, 5_000), 10.0);
        assert_eq!(projected_core_pct(6.0, 6, 5_000), 60.0);
        // At most one read-ahead per 20 s: 3 a minute, whatever the focus rate.
        assert_eq!(projected_core_pct(6.0, 6, 20_000), 30.0);
        assert_eq!(projected_core_pct(6.0, 4, 0), 40.0);
    }

    #[test]
    fn fake_provider_models_a_whole_prompt_prefill() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let p = FakeChat::new(Locality::OnDevice, FakeLatency::default());
            let rules = "Draft a reply in the user's voice. ".repeat(20);
            let thread = "Can we move the review to Friday afternoon? ".repeat(20);
            // A read-ahead: the whole request, ending with a cache breakpoint.
            let mut ahead = chat_req(&rules, &thread);
            ahead.messages[0].content.push(Part::CacheBreakpoint);
            p.prewarm_prefix(&ahead).await.unwrap();
            // The trigger's request: everything but the breakpoint is served from the cache.
            let (input, cached) = usage(&p, chat_req(&rules, &thread)).await;
            assert_eq!(cached, input);
        });
    }

    #[test]
    fn task_selection() {
        assert_eq!(spread(10, 3), [0, 3, 6]);
        assert_eq!(unused(5, &[0, 2], 2), [4, 3]);
        assert_eq!(unused(2, &[0, 1], 1), [0]);
    }
}
