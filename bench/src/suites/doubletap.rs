//! `bench/doubletap`: Double Tap latency through the real drafting engine (milestone M3 exit
//! criterion: time to the first insertable token).
//!
//! Every draft goes through [`nugget_draft::Drafter`], built as `nuggetd` builds it
//! (`crates/nuggetd/src/features/draft.rs`): the default [`DraftConfig`] (1,500-token retrieval
//! budget, Synthesis tier), a provider warmer that calls [`Provider::prewarm`], retrieval run on
//! a blocking thread, over one in-memory [`Store`] per persona holding the `draft` fixture
//! corpus (`bench/fixtures/draft/`: every knowledge item, sensitive ones flagged, plus the
//! persona's sent messages). Retrieval is **text-only** (no embedder). Voice hints come from the
//! persona's sent messages (channel or recipient style statistics plus three exemplars), so the
//! prompt has the size and shape of a real one.
//!
//! Per task, as the desktop shell does on a trigger (`apps/desktop/src-tauri/src/doubletap/
//! flow.rs`): [`Drafter::prewarm`] with an app-only hint (the shell prewarms before it has read
//! the field and thread, so retrieval is not started early), then [`Drafter::begin`] with the
//! task's full [`DraftContext`], then the [`nugget_draft::DraftStream`] is drained and its
//! [`DraftTimings`] read. Every time is measured from the start of `begin`; the accessibility
//! read of the field and the IPC hop to the daemon are not included.
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
//! - `total.p50/.p95`: until the stream finished.
//! - `retrieval.p50`: how long `begin` waited for retrieval and the voice lookup.
//! - `requests`, `completed`, `failures`, `timeouts`, `empty_drafts` (completed with no visible
//!   text), `retrieval_skipped` (retrieval timed out or failed, so the draft had no context):
//!   counts. Percentiles cover completed drafts only; failed and timed-out drafts are counted,
//!   never hidden. The suite fails only when no draft completed.
//! - `output_tokens_per_s` (provider-reported output tokens over the time after the first
//!   insertable token) and `prompt_tokens.mean` (provider-reported), when the provider reports
//!   usage.
//! - `cold.*`: one untimed warm-up draft first (model load, first KV fill), reported apart.
//! - `thinking_model_default.*` (`--local-models` only): [`THINKING_TASKS`] drafts with the
//!   model's own thinking default ([`Thinking::Model`]), showing what Qwen3's default thinking
//!   costs, and how many drafts it left without visible text.
//!
//! Models: `--local-models` runs the signed manifest's smallest chat model through
//! `nugget-runtime` ([`LocalModels`]); else `--provider-url` (with `--provider-model`,
//! `--provider-kind`, `--api-key-env`, as for `draft`); else a deterministic in-process fake
//! provider whose latency comes from `--fake-ttft-ms`, `--fake-prefill-ms-per-1k` and
//! `--fake-token-ms`, and whose locality from `--draft-locality`, so the suite runs in CI.

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
    DraftConfig, DraftTimings, Drafter, RetrievalRunner, VoiceQuery, VoiceSource, Warmer,
};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Part, Provider, ProviderConfig, ProviderKind, ProviderMeta,
    Router, StopReason, build_provider,
};
use nugget_retrieve::{PackedContext, RetrieveConfig, Retriever};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use nugget_voice::{Channel, VoiceHints};
use serde_json::json;
use time::{Date, OffsetDateTime, Time};

use crate::local::{HostInfo, LocalModels, Thinking};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, Benchmark, RunContext};
use crate::stats::summarize;
use crate::suites::draft::fixtures::{Corpus, Persona, Task};
use crate::suites::draft::{
    DraftLocality, DraftOptions, TTFT_TARGET_CLOUD_S, TTFT_TARGET_LOCAL_S, style,
};

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
/// the prompt, with usage in `Done`. Can also fail or stall on request (tests).
pub struct FakeChat {
    id: ProviderId,
    locality: Locality,
    latency: FakeLatency,
    /// Every `n`-th request (1-based) fails before streaming.
    fail_every: Option<usize>,
    /// Every request opens a stream that never yields.
    stall: bool,
    requests: AtomicUsize,
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
}

/// Estimated prompt tokens of a request (system plus text parts).
fn prompt_tokens(req: &ChatRequest) -> usize {
    let mut n = req
        .system
        .as_deref()
        .map_or(0, nugget_extract::estimate_tokens);
    for m in &req.messages {
        for p in &m.content {
            if let Part::Text { text } = p {
                n += nugget_extract::estimate_tokens(text);
            }
        }
    }
    n
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

    async fn chat(&self, req: ChatRequest) -> CoreResult<ChatStream> {
        let n = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_every.is_some_and(|k| n.is_multiple_of(k)) {
            return Err(Error::Provider("fake provider: injected failure".into()));
        }
        if self.stall {
            return Ok(futures::stream::pending().boxed());
        }
        let input = prompt_tokens(&req);
        let first = Duration::from_millis(
            self.latency.ttft_ms + self.latency.prefill_ms_per_1k * input as u64 / 1000,
        );
        let per_token = Duration::from_millis(self.latency.token_ms);
        let words: Vec<String> = FAKE_REPLY.split_inclusive(' ').map(str::to_owned).collect();
        let output = u32::try_from(words.len()).unwrap_or(u32::MAX);
        let input = u32::try_from(input).unwrap_or(u32::MAX);
        let stream = futures::stream::unfold(0usize, move |i| {
            let words = words.clone();
            async move {
                if i > words.len() {
                    return None;
                }
                let delay = if i == 0 { first } else { per_token };
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let ev = match words.get(i) {
                    Some(w) => ChatEvent::TextDelta(w.clone()),
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

/// Opens the provider's connection on trigger (as `nuggetd`'s `ProviderWarmer`).
struct ProviderWarmer;

#[async_trait]
impl Warmer for ProviderWarmer {
    async fn warm(&self, provider: Arc<dyn Provider>) {
        let _ = provider.prewarm().await;
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
fn build_stores(corpus: &Corpus) -> Result<BTreeMap<String, Store>, BenchError> {
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

/// One drafter per persona, all routing to `provider` on the Synthesis tier.
fn build_drafters(
    corpus: &Corpus,
    stores: &BTreeMap<String, Store>,
    provider: Arc<dyn Provider>,
) -> HashMap<String, Drafter> {
    let tiers = HashMap::from([(TIER, vec![provider.id().clone()])]);
    let router = Arc::new(Router::new(vec![provider], tiers));
    let now = crate::suites::draft::arms::reference_time();
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
            DraftConfig::default(),
        )
        .with_warmer(Arc::new(ProviderWarmer))
        .with_retrieval_runner(Arc::new(BlockingRetrieval));
        out.insert(p.id.clone(), drafter);
    }
    out
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
}

/// How one draft ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Completed(Sample),
    Failed(String),
    TimedOut,
}

/// Prewarm, begin, drain. Bounded by `timeout` end to end.
async fn one_draft(drafter: &Drafter, task: &Task, timeout: Duration) -> Outcome {
    let hint = DraftContext {
        app: task.context.app.clone(),
        ..DraftContext::default()
    };
    drafter.prewarm(&hint);
    let req = DraftRequest {
        context: task.context.clone(),
        instruction: None,
        provider: None,
        max_tokens: None,
    };
    let work = async {
        let (_, mut stream) = drafter.begin(req).await?;
        let mut sample = Sample::default();
        while let Some(ev) = stream.next().await {
            match ev? {
                ChatEvent::TextDelta(d) => sample.text_chars += d.chars().count(),
                ChatEvent::Stats(s) => {
                    sample.input_tokens = sample.input_tokens.or(s.input_tokens);
                    sample.output_tokens = sample.output_tokens.or(s.output_tokens);
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

/// Every draft of one pass, in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pass {
    pub outcomes: Vec<Outcome>,
}

impl Pass {
    pub fn samples(&self) -> impl Iterator<Item = &Sample> {
        self.outcomes.iter().filter_map(|o| match o {
            Outcome::Completed(s) => Some(s),
            _ => None,
        })
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

async fn run_pass(
    drafters: &HashMap<String, Drafter>,
    corpus: &Corpus,
    tasks: &[usize],
    timeout: Duration,
    label: &str,
) -> Pass {
    let mut pass = Pass::default();
    for (n, &ti) in tasks.iter().enumerate() {
        let task = &corpus.tasks[ti];
        let outcome = match drafters.get(&task.persona) {
            Some(d) => one_draft(d, task, timeout).await,
            None => Outcome::Failed(format!("no drafter for persona {}", task.persona)),
        };
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

fn describe(o: &Outcome) -> String {
    match o {
        Outcome::Completed(s) => format!(
            "first inserted token {}, total {}, {} chars",
            s.timings
                .first_token_ms
                .map_or_else(|| "none".into(), |ms| format!("{ms} ms")),
            s.timings
                .total_ms
                .map_or_else(|| "?".into(), |ms| format!("{ms} ms")),
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

fn values(pass: &Pass, f: impl Fn(&DraftTimings) -> Option<u64>) -> Vec<f64> {
    pass.samples().filter_map(|s| secs(f(&s.timings))).collect()
}

fn count(name: String, v: usize, dir: Direction, desc: String) -> Metric {
    Metric::new(name, v as f64, "drafts", dir, desc)
}

/// Metrics of the main pass. `target`: the first-insertable-token p50 target, if any.
pub fn pass_metrics(pass: &Pass, target: Option<f64>) -> Vec<Metric> {
    let p = "doubletap";
    let mut out = Vec::new();
    let latencies: [(&str, Vec<f64>, &str); 3] = [
        (
            "ttft",
            values(pass, |t| t.provider_open_ms),
            "until the provider's first stream event (prompt, retrieval and voice included)",
        ),
        (
            "first_inserted_token",
            values(pass, |t| t.first_token_ms),
            "until the first post-processed text the overlay shows and the user can insert \
             (insertion itself not timed); drafts with no visible text excluded",
        ),
        (
            "total",
            values(pass, |t| t.total_ms),
            "until the draft stream finished",
        ),
    ];
    for (name, v, what) in latencies {
        let Some(s) = summarize(&v) else { continue };
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
    if let Some(s) = summarize(&values(pass, |t| t.retrieval_ms)) {
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
        ("first_inserted_token", values(pass, |t| t.first_token_ms)),
        ("ttft", values(pass, |t| t.provider_open_ms)),
        ("total", values(pass, |t| t.total_ms)),
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

// ---------------------------------------------------------------------------------------------
// The suite

/// The provider for `--provider-url` (the `draft` suite's config logic).
fn url_provider(o: &DraftOptions, url: &str) -> Result<(Arc<dyn Provider>, String), BenchError> {
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
    let cfg: ProviderConfig = serde_json::from_value(cfg)
        .map_err(|e| BenchError::Failed(format!("doubletap provider config: {e}")))?;
    let provider = build_provider(&cfg, Arc::new(NullEgress))?;
    Ok((provider, format!("{kind} {url} model {model}")))
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
        let provider = models.chat()?;
        let thinking = if models.thinking == Thinking::Model {
            // The main pass already thinks at the model's default; no second pass needed.
            None
        } else {
            Some(models.chat_with(Thinking::Model)?)
        };
        let desc = models.chat_label();
        return Ok(Setup {
            exec: Exec::Local(Box::new(models)),
            provider,
            thinking,
            real: true,
            desc,
        });
    }
    let opts = ctx.config.draft.clone();
    if let Some(url) = &opts.provider_url {
        let (provider, desc) = url_provider(&opts, url)?;
        return Ok(Setup {
            exec: Exec::Own(own_runtime()?),
            provider,
            thinking: None,
            real: true,
            desc,
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
    })
}

/// `n` indices out of `0..all`, evenly spread (all of them when `n >= all`).
fn spread(all: usize, n: usize) -> Vec<usize> {
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
    let all = corpus.tasks.len();
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

    ctx.param("model", s.desc.clone());
    ctx.param("model.locality", format!("{locality:?}"));
    ctx.param("engine", "nugget_draft::Drafter (DraftConfig::default)");
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
    ctx.param("prompt_budget_tokens", DraftConfig::default().budget_tokens);
    ctx.param("retrieval", RETRIEVAL_DESC);
    ctx.param("prewarm", PREWARM_DESC);
    ctx.param("voice", VOICE_DESC);
    ctx.param(
        "timed_from",
        "Drafter::begin (a11y read and IPC not included)",
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
    let drafters = build_drafters(&corpus, &stores, s.provider.clone());
    let started = Instant::now();
    let (cold, pass) = s.exec.block_on(async {
        let cold = run_pass(&drafters, &corpus, &warmup, timeout, "warm-up").await;
        let pass = run_pass(&drafters, &corpus, &tasks, timeout, "timed").await;
        (cold, pass)
    });
    ctx.param("main_pass_s", round3(started.elapsed().as_secs_f64()));
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
    if let Some(c) = cold.outcomes.first() {
        out.extend(cold_metrics(c));
    }

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
            // Tasks the main pass did not draft, so no prompt-cache reuse flatters it.
            let thinking_tasks = unused(all, &[tasks.as_slice(), &warmup].concat(), THINKING_TASKS);
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
            let drafters = build_drafters(&corpus, &stores, provider.clone());
            let thinking = s.exec.block_on(run_pass(
                &drafters,
                &corpus,
                &thinking_tasks,
                timeout,
                "thinking",
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
        ] {
            assert!(r.metric(name).is_some(), "missing {name}");
        }
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
        assert_eq!(value(&r, "doubletap.empty_drafts"), 0.0);
        // Loopback is on-device: the local target applies.
        assert_eq!(r.config.params["model.locality"], "OnDevice");
        assert_eq!(r.targets[0].target, TTFT_TARGET_LOCAL_S);
        let sent = rt.block_on(server.received_requests()).unwrap();
        let drafts = sent
            .iter()
            .filter(|q| q.url.path().ends_with("/chat/completions"))
            .count();
        assert_eq!(drafts, 5, "warm-up + 4 timed drafts");
    }

    #[test]
    fn task_selection() {
        assert_eq!(spread(10, 3), [0, 3, 6]);
        assert_eq!(unused(5, &[0, 2], 2), [4, 3]);
        assert_eq!(unused(2, &[0, 1], 1), [0]);
    }
}
