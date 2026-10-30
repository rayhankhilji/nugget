//! `bench/draft`: Double Tap reply drafting (milestone M3), Nugget versus a Sentient-style
//! baseline. See `bench/README.md` for the methodology.
//!
//! Both arms write the same prompt format ([`prompt`]) for the same model; only the content
//! differs ([`arms`]). The default model is the deterministic [`fake`] one, so CI numbers are
//! reproducible; `--provider-url` runs a real model through `nugget-infer`, and `--local-models`
//! the manifest's local chat model through `nugget-runtime`, as the app does.
//!
//! With a real model every request has a timeout; a failed, timed-out or context-overflow
//! request is counted (`draft.<arm>.failures`) and its draft scored as empty, and the suite
//! fails only when no request worked. When the model's context window is known (or
//! `--draft-context-tokens` is given) the baseline's whole-vault prompt is cut to fit, as
//! Sentient's own cap does. [`judge`] adds a pairwise judge (`--draft-judge`), on the drafting
//! model or, with `--draft-judge-model`, on a second local model; the rule-based judge always
//! runs next to it (`draft.judge_heuristic.*`).
//!
//! With a real model the Nugget arm drafts through the real engine ([`engine`]), so
//! `DraftConfig::grounding` applies: `--draft-grounding both` (the default with a real model)
//! runs it twice on the same tasks, grounded (`draft.nugget.*`) and not
//! (`draft.nugget_ungrounded.*`). Drafts use a fixed temperature (`--draft-temperature`, 0 by
//! default); `ChatRequest` has no seed field, so no seed is sent.
//!
//! A limited run (a real model below the full corpus) spreads its tasks evenly over the first
//! [`SPREAD_TASKS`] tasks, so it drafts the same tasks as runs before the corpus grew, and above
//! `smoke` adds every task tagged [`ARTEFACT_TAG`] (invented attachments, links and invites, EE2).

pub mod arms;
pub mod engine;
pub mod fake;
pub mod fixtures;
pub mod judge;
pub mod prompt;
pub mod score;
pub mod style;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use nugget_core::capability::Locality;
use nugget_core::egress::NullEgress;
use nugget_core::ipc::RuntimeRole;
use nugget_infer::{ChatEvent, ChatRequest, Message, Provider, ProviderConfig, build_provider};
use serde_json::json;

use crate::local::{LocalModels, LocalOptions, Thinking};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, Benchmark, RunContext};
use arms::{BaselineArm, NuggetArm, World};
use engine::Engine;
use fixtures::Corpus;
use judge::{JudgeInput, JudgeMode, JudgeSummary, ModelJudge, TaskJudgement};
use prompt::Prompt;
use score::{ArmSummary, ScoreInput, TaskScore};

/// Output cap for a draft (a thinking model gets room to think first).
pub const DRAFT_MAX_TOKENS: u32 = 400;
pub const DRAFT_MAX_TOKENS_THINKING: u32 = 2048;
/// Output cap for a judge verdict.
pub const JUDGE_MAX_TOKENS: u32 = 160;
/// Context window assumed for `--local-models` when the runtime config does not state one
/// (`nugget-runtime`'s chat default on a 16 GB machine).
pub const DEFAULT_LOCAL_CONTEXT_TOKENS: u32 = 8192;
/// Tokens kept free besides the output: chat template, role markers, special tokens.
pub const CONTEXT_MARGIN_TOKENS: u32 = 256;
/// `estimate_tokens` counts four characters a token; real tokenizers count more tokens for
/// numbers, addresses and punctuation-heavy text, so only this share of the window is filled.
pub const ESTIMATE_SAFETY: f64 = 0.85;

/// Tasks a limited run spreads over: the corpus before EE2 appended the `artefact` tasks. Keeps
/// the tasks earlier real-model runs drafted (for example the 16 of run 41007073954) the same.
pub const SPREAD_TASKS: usize = 66;
/// Tag of the invented-artefact traps and controls, which every limited run includes.
pub const ARTEFACT_TAG: &str = "artefact";

/// The tasks a run drafts (indices into the corpus): every task when `limit` covers the corpus;
/// otherwise `limit` tasks evenly spread over the first [`SPREAD_TASKS`], plus (with `artefact`;
/// every scale but `smoke`, which stays a few tasks) every task tagged [`ARTEFACT_TAG`], in
/// corpus order.
pub fn select_tasks(corpus: &Corpus, limit: usize, artefact: bool) -> Vec<usize> {
    let all = corpus.tasks.len();
    if limit >= all {
        return (0..all).collect();
    }
    let mut picks = spread(SPREAD_TASKS.min(all), limit);
    if artefact {
        picks.extend((0..all).filter(|i| corpus.tasks[*i].has_tag(ARTEFACT_TAG)));
    }
    picks.sort_unstable();
    picks.dedup();
    picks
}

/// The baseline's prompt budget in estimated tokens for a context window: window minus the
/// output cap minus [`CONTEXT_MARGIN_TOKENS`], times [`ESTIMATE_SAFETY`].
pub fn prompt_budget(context_tokens: u32, max_output: u32) -> usize {
    let room = context_tokens.saturating_sub(max_output + CONTEXT_MARGIN_TOKENS);
    (f64::from(room) * ESTIMATE_SAFETY) as usize
}

/// Exit-criterion targets for time to first token, p50 (docs/m3-spec.md).
pub const TTFT_TARGET_CLOUD_S: f64 = 1.2;
pub const TTFT_TARGET_LOCAL_S: f64 = 3.0;

/// Which path produced the Nugget arm. Switches to the real crates once `nugget-draft` (M3-C)
/// exposes its prompt builder and `nugget-voice` (M3-D) its profile and `style_distance`.
pub const NUGGET_PATH: &str = "in_suite: nugget_draft::classify (contract rules) + nugget-retrieve \
     (text-only, recency boost, sensitive items excluded for cloud) + voice-stats prompt \
     (nugget_voice::VoiceHints/StyleStats computed in-suite) + instruction neutralising";
pub const BASELINE_PATH: &str = "sentient_style: whole vault (600 KB cap) + 5 raw samples x 6 \
     recent contacts + 15 recent emails + raw thread; text only (no screenshot, generous to the \
     baseline)";
pub const NUGGET_ENGINE_PATH: &str = "engine: nugget_draft::Drafter as nuggetd builds it \
     (DraftConfig::default except grounding and temperature; text-only retrieval over the \
     persona's store, default RetrieveConfig, fixed reference time; voice from the persona's sent \
     messages, recipient or channel statistics + 3 exemplars)";
pub const SEED_NOTE: &str = "not sent: ChatRequest has no seed field, so the provider samples \
     with its own seed (fixed temperature instead)";
pub const VOICE_METRIC: &str = "in_suite stylometric distance (bench/src/suites/draft/style.rs); \
     switches to nugget_voice::style_distance when it lands";

/// Where the fake model pretends to run. Real providers report their own locality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, clap::ValueEnum)]
pub enum DraftLocality {
    /// A cloud model: sensitive items must stay out of prompts and drafts.
    #[default]
    Cloud,
    /// An on-device model: sensitive items may be used.
    Local,
}

/// `draft` options (flattened into the CLI).
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct DraftOptions {
    /// `draft`: base URL of a real chat provider (e.g. `https://api.anthropic.com` or
    /// `http://127.0.0.1:8080/v1`) instead of the deterministic fake model.
    #[arg(long, value_name = "URL")]
    pub provider_url: Option<String>,
    /// `draft`: model name for `--provider-url`.
    #[arg(long, value_name = "MODEL", requires = "provider_url")]
    pub provider_model: Option<String>,
    /// `draft`: environment variable holding the provider's API key.
    #[arg(long, value_name = "VAR", requires = "provider_url")]
    pub api_key_env: Option<String>,
    /// `draft`: wire protocol of `--provider-url`: `anthropic`, `openai_responses` or
    /// `openai_compatible` (default).
    #[arg(long, value_name = "KIND", requires = "provider_url")]
    pub provider_kind: Option<String>,
    /// `draft`: where the fake model counts as running (sets the sensitive-content gate).
    #[arg(long, value_enum, default_value_t = DraftLocality::Cloud)]
    pub draft_locality: DraftLocality,
    /// `draft`: fake model's fixed time to first token, in ms.
    #[arg(long, value_name = "MS", default_value_t = 0)]
    pub fake_ttft_ms: u64,
    /// `draft`: fake model's prefill cost per 1,000 prompt tokens, in ms.
    #[arg(long, value_name = "MS", default_value_t = 0)]
    pub fake_prefill_ms_per_1k: u64,
    /// `draft`: fake model's time per output token, in ms.
    #[arg(long, value_name = "MS", default_value_t = 0)]
    pub fake_token_ms: u64,
    /// `draft`: write every draft with its scores to this JSONL file (for spot checks).
    #[arg(long, value_name = "FILE")]
    pub draft_dump: Option<std::path::PathBuf>,
    /// `draft`: context window of a real model, in tokens, for cutting the baseline's
    /// whole-vault prompt to fit (default: the provider config's `context_tokens`; with
    /// `--local-models` 8192 if that is unset; otherwise no cut).
    #[arg(long, value_name = "TOKENS")]
    pub draft_context_tokens: Option<u32>,
    /// `draft`: per-request timeout for a real model (drafts and judge verdicts), in seconds.
    #[arg(long, value_name = "SECS")]
    pub draft_timeout_s: Option<u64>,
    /// `draft`: pairwise judge: `model` (the drafting model, both orders), `heuristic`
    /// (deterministic, from the suite's scores) or `none`. Default: `model` with
    /// `--local-models`, `none` otherwise.
    #[arg(long, value_enum)]
    pub draft_judge: Option<JudgeMode>,
    /// `draft`: run the Nugget arm with grounding `on`, `off`, or `both` (two Nugget arms on
    /// the same tasks; the grounded one is `draft.nugget.*`, the other
    /// `draft.nugget_ungrounded.*`). Default: `both` with a real model, `on` otherwise.
    #[arg(long, value_enum)]
    pub draft_grounding: Option<GroundingMode>,
    /// `draft`: sampling temperature of every draft request, both arms (default 0 with a real
    /// model; the fake model is deterministic).
    #[arg(long, value_name = "T")]
    pub draft_temperature: Option<Temperature>,
    /// `draft`: manifest id of a different local model to judge with (`--draft-judge model`
    /// with `--local-models`); started as a second local server next to the drafting one.
    #[arg(long, value_name = "ID")]
    pub draft_judge_model: Option<String>,
    /// `draft`: per-request timeout of the model judge, in seconds (default 240 with
    /// `--draft-judge-model`, else the draft timeout).
    #[arg(long, value_name = "SECS")]
    pub draft_judge_timeout_s: Option<u64>,
    /// `draft`: also judge the grounded Nugget arm against the ungrounded one (needs
    /// `--draft-grounding both` and a judge; doubles the judge's requests).
    #[arg(long)]
    pub draft_judge_grounding: bool,
    /// `draft`, `doubletap`: the smaller prompt budget `nugget-draft` gives non-cloud providers
    /// (`nugget_draft::LocalBudget`): `on` (default, as the app) or `off` (the cloud budget for
    /// every provider), for before/after comparisons on one job.
    #[arg(long, value_enum, default_value_t = Switch::On)]
    pub draft_local_budget: Switch,
    /// `draft`, `doubletap`: the streaming check of grounded drafts
    /// (`DraftConfig::grounding_check`): `secrets` (stream word by word, stop a copied
    /// credential) or `sentences` (hold and check whole sentences). Default: the product's.
    #[arg(long, value_enum)]
    pub draft_grounding_check: Option<CheckMode>,
}

/// `--draft-grounding-check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum CheckMode {
    Sentences,
    Secrets,
}

impl CheckMode {
    pub fn check(self) -> nugget_draft::GroundingCheck {
        match self {
            Self::Sentences => nugget_draft::GroundingCheck::Sentences,
            Self::Secrets => nugget_draft::GroundingCheck::Secrets,
        }
    }
}

/// The streaming check a run uses: the option, else the product default.
pub fn grounding_check(opts: &DraftOptions) -> nugget_draft::GroundingCheck {
    opts.draft_grounding_check
        .map_or_else(nugget_draft::GroundingCheck::default, CheckMode::check)
}

/// `GroundingCheck` as recorded in reports.
pub fn check_name(c: nugget_draft::GroundingCheck) -> &'static str {
    match c {
        nugget_draft::GroundingCheck::Sentences => "sentences",
        nugget_draft::GroundingCheck::Secrets => "secrets",
    }
}

/// `--draft-grounding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum GroundingMode {
    /// One Nugget arm, grounding on (the product default).
    On,
    /// One Nugget arm, grounding off (reported as `draft.nugget.*`).
    Off,
    /// Both, side by side on the same tasks.
    Both,
}

impl GroundingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::Both => "both",
        }
    }

    /// The default: `both` with a real model, `on` with the fake one (whose in-suite Nugget
    /// arm has no grounding switch).
    pub fn resolve(opt: Option<Self>, real: bool) -> Self {
        opt.unwrap_or(if real { Self::Both } else { Self::On })
    }
}

/// A sampling temperature in `[0, 2]` (`--draft-temperature`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Temperature(pub f32);

impl Eq for Temperature {}

impl std::str::FromStr for Temperature {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let t: f32 = s
            .trim()
            .parse()
            .map_err(|_| format!("`{s}` is not a number"))?;
        if !(0.0..=2.0).contains(&t) {
            return Err(format!("temperature {t} is outside [0, 2]"));
        }
        Ok(Self(t))
    }
}

/// Default temperature of draft requests to a real model.
pub const DEFAULT_TEMPERATURE: f32 = 0.0;
/// Default `--draft-judge-timeout-s` with `--draft-judge-model`: an 8B judge on 2 CPU cores
/// reads about 14 tokens/s, so a ~700-token judge prompt takes ~50 s before it writes its
/// verdict at a few tokens/s.
pub const DEFAULT_JUDGE_TIMEOUT_S: u64 = 240;

/// Where the judge's verdicts come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgePlan {
    None,
    Heuristic,
    /// The drafting model.
    SameModel,
    /// A second local server with this manifest model.
    LocalModel(String),
}

impl JudgePlan {
    pub fn mode(&self) -> JudgeMode {
        match self {
            Self::None => JudgeMode::None,
            Self::Heuristic => JudgeMode::Heuristic,
            Self::SameModel | Self::LocalModel(_) => JudgeMode::Model,
        }
    }
}

/// Decide the judge from the options: `--draft-judge` (default `model` with `--local-models`
/// or a `--draft-judge-model`, `none` otherwise), `--draft-judge-model`, whether the drafts
/// come from a real model and from a local one, and the drafting model's manifest id.
pub fn plan_judge(
    mode: Option<JudgeMode>,
    judge_model: Option<&str>,
    real: bool,
    local: bool,
    draft_model_id: Option<&str>,
) -> Result<JudgePlan, String> {
    let mode = mode.unwrap_or(if local || judge_model.is_some() {
        JudgeMode::Model
    } else {
        JudgeMode::None
    });
    if judge_model.is_some() && mode != JudgeMode::Model {
        return Err("--draft-judge-model needs --draft-judge model".into());
    }
    match mode {
        JudgeMode::None => Ok(JudgePlan::None),
        JudgeMode::Heuristic => Ok(JudgePlan::Heuristic),
        JudgeMode::Model if !real => Err("--draft-judge model needs a real model \
             (--local-models or --provider-url); the fake model can use --draft-judge heuristic"
            .into()),
        JudgeMode::Model => match judge_model {
            None => Ok(JudgePlan::SameModel),
            Some(_) if !local => Err("--draft-judge-model needs --local-models (the judge runs \
                 on a second local server)"
                .into()),
            Some(id) if Some(id) == draft_model_id => Ok(JudgePlan::SameModel),
            Some(id) => Ok(JudgePlan::LocalModel(id.to_owned())),
        },
    }
}

/// An on/off CLI switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, clap::ValueEnum)]
pub enum Switch {
    #[default]
    On,
    Off,
}

impl Switch {
    pub fn is_on(self) -> bool {
        self == Self::On
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

/// Default `--draft-timeout-s`: a 4B model on 4 CPU cores prefills a ~6k-token baseline prompt
/// in a few minutes.
pub const DEFAULT_TIMEOUT_S: u64 = 420;

struct RealModel {
    provider: Arc<dyn Provider>,
    timeout: Duration,
    max_output: u32,
    /// Sampling temperature of every draft request.
    temperature: Option<f32>,
    /// The servers behind `provider` (`--local-models`); dropping it stops them.
    _local: Option<Box<LocalModels>>,
}

enum Model {
    Fake(fake::Latency),
    Real(RealModel),
}

pub(crate) struct Generation {
    text: String,
    ttft: Duration,
    total: Duration,
}

/// A real-model request that produced no draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestFailure {
    /// `timeout`, `context_overflow` or `error`.
    pub kind: &'static str,
    pub message: String,
    pub elapsed: Duration,
}

/// Run a generation (`work` yields the text and the time of its first visible token) within
/// `timeout`; a timeout or an error becomes a [`RequestFailure`]. Times are from `started`.
async fn timed<F>(
    work: F,
    timeout: Duration,
    started: Instant,
) -> Result<Generation, RequestFailure>
where
    F: Future<Output = Result<(String, Option<Duration>), nugget_core::Error>>,
{
    let fail = |kind: &'static str, message: String| RequestFailure {
        kind,
        message,
        elapsed: started.elapsed(),
    };
    let (text, ttft) = match tokio::time::timeout(timeout, work).await {
        Err(_) => {
            return Err(fail(
                "timeout",
                format!("no complete draft within {} s", timeout.as_secs()),
            ));
        }
        Ok(Err(e)) => {
            let message = e.to_string();
            return Err(fail(failure_kind(&message), message));
        }
        Ok(Ok(r)) => r,
    };
    let total = started.elapsed();
    // A thinking model may inline its reasoning; no user would send that.
    Ok(Generation {
        text: judge::strip_think(&text).trim().to_owned(),
        ttft: ttft.unwrap_or(total),
        total,
    })
}

/// Classify a provider error (llama-server: "the request exceeds the available context size";
/// OpenAI: "maximum context length"; Anthropic: "prompt is too long").
pub fn failure_kind(message: &str) -> &'static str {
    let m = message.to_lowercase();
    let overflow = (m.contains("context") || m.contains("n_ctx"))
        && [
            "exceed",
            "too long",
            "too large",
            "maximum",
            "overflow",
            "length",
        ]
        .iter()
        .any(|w| m.contains(w))
        || m.contains("prompt is too long");
    if overflow {
        "context_overflow"
    } else {
        "error"
    }
}

struct Built {
    model: Model,
    locality: Locality,
    desc: String,
    /// Context window from the provider config, if it declares one.
    context_tokens: Option<u32>,
    local: bool,
    /// Manifest id of the local chat model (`--local-models`).
    model_id: Option<String>,
}

impl Model {
    fn build(
        o: &DraftOptions,
        local: &LocalOptions,
        ctx: &mut RunContext,
    ) -> Result<Built, BenchError> {
        let timeout = Duration::from_secs(o.draft_timeout_s.unwrap_or(DEFAULT_TIMEOUT_S).max(1));
        let temperature = Some(o.draft_temperature.map_or(DEFAULT_TEMPERATURE, |t| t.0));
        if local.local_models {
            if o.provider_url.is_some() {
                return Err(BenchError::Failed(
                    "--local-models and --provider-url are alternatives; pass one".into(),
                ));
            }
            let models = LocalModels::start(local, &[RuntimeRole::Chat])?;
            models.record(ctx);
            let provider = models.chat()?;
            let meta = provider.meta();
            let desc = models.chat_label();
            let model_id = models.chat.as_ref().map(|s| s.entry.id.clone());
            let max_output = if models.thinking == Thinking::Model {
                DRAFT_MAX_TOKENS_THINKING
            } else {
                DRAFT_MAX_TOKENS
            };
            return Ok(Built {
                model: Self::Real(RealModel {
                    provider,
                    timeout,
                    max_output,
                    temperature,
                    _local: Some(Box::new(models)),
                }),
                locality: meta.locality,
                desc,
                context_tokens: meta.context_tokens,
                local: true,
                model_id,
            });
        }
        let Some(url) = &o.provider_url else {
            let latency = fake::Latency {
                ttft_ms: o.fake_ttft_ms,
                prefill_ms_per_1k: o.fake_prefill_ms_per_1k,
                token_ms: o.fake_token_ms,
            };
            let locality = match o.draft_locality {
                DraftLocality::Cloud => Locality::Cloud,
                DraftLocality::Local => Locality::OnDevice,
            };
            return Ok(Built {
                model: Self::Fake(latency),
                locality,
                desc: "fake-deterministic".into(),
                context_tokens: None,
                local: false,
                model_id: None,
            });
        };
        let model = o.provider_model.clone().unwrap_or_else(|| "default".into());
        let kind = o
            .provider_kind
            .clone()
            .unwrap_or_else(|| "openai_compatible".into());
        let mut cfg = json!({
            "id": "bench-draft",
            "kind": kind,
            "base_url": url,
            "model": model,
        });
        if let Some(var) = &o.api_key_env {
            cfg["api_key"] = json!(format!("env:{var}"));
        }
        let cfg: ProviderConfig = serde_json::from_value(cfg)
            .map_err(|e| BenchError::Failed(format!("draft provider config: {e}")))?;
        let provider = build_provider(&cfg, Arc::new(NullEgress))?;
        let meta = provider.meta();
        Ok(Built {
            model: Self::Real(RealModel {
                provider,
                timeout,
                max_output: DRAFT_MAX_TOKENS,
                temperature,
                _local: None,
            }),
            locality: meta.locality,
            desc: format!("{kind} {url} model {model}"),
            context_tokens: meta.context_tokens,
            local: false,
            model_id: None,
        })
    }

    fn max_output(&self) -> u32 {
        match self {
            Self::Fake(_) => DRAFT_MAX_TOKENS,
            Self::Real(r) => r.max_output,
        }
    }

    /// Generate a reply. Times are measured from `started` (before the prompt was built).
    async fn generate(&self, p: &Prompt, started: Instant) -> Result<Generation, RequestFailure> {
        match self {
            Self::Fake(latency) => {
                let text = fake::draft(&p.system, &p.user);
                tokio::time::sleep(latency.first_token(p.tokens())).await;
                let ttft = started.elapsed();
                tokio::time::sleep(latency.rest(nugget_extract::estimate_tokens(&text))).await;
                Ok(Generation {
                    text,
                    ttft,
                    total: started.elapsed(),
                })
            }
            Self::Real(real) => {
                let mut req = ChatRequest::new(vec![Message::user_text(p.user.clone())])
                    .with_system(p.system.clone())
                    .with_max_tokens(real.max_output)
                    .with_feature("draft");
                req.temperature = real.temperature;
                let stream = async {
                    let mut stream = real.provider.chat(req).await?;
                    let mut text = String::new();
                    let mut ttft = None;
                    while let Some(ev) = stream.next().await {
                        match ev? {
                            ChatEvent::TextDelta(d) => {
                                if ttft.is_none() && !d.trim().is_empty() {
                                    ttft = Some(started.elapsed());
                                }
                                text.push_str(&d);
                            }
                            ChatEvent::Done { .. } => break,
                            _ => {}
                        }
                    }
                    Ok::<_, nugget_core::Error>((text, ttft))
                };
                timed(stream, real.timeout, started).await
            }
        }
    }
}

/// Everything a run produced, before it becomes metrics.
#[derive(Default)]
pub struct DraftRun {
    pub nugget: Vec<TaskScore>,
    pub baseline: Vec<TaskScore>,
    pub nugget_drafts: Vec<String>,
    pub baseline_drafts: Vec<String>,
    /// Per task: the failure of each arm's request, if it failed.
    pub nugget_errors: Vec<Option<RequestFailure>>,
    pub baseline_errors: Vec<Option<RequestFailure>>,
    /// The second Nugget arm (`--draft-grounding both`: grounding off); empty otherwise.
    pub ungrounded: Vec<TaskScore>,
    pub ungrounded_drafts: Vec<String>,
    pub ungrounded_errors: Vec<Option<RequestFailure>>,
    /// Tasks whose baseline prompt was cut to the budget.
    pub baseline_truncated: usize,
    pub field_kind_correct: usize,
    pub recipient_profiles: usize,
    pub excluded_sensitive: u32,
    pub neutralised: usize,
    /// Per task, when a judge ran.
    pub judgements: Vec<TaskJudgement>,
}

/// Arm name of the second Nugget arm in metrics, failures and the draft dump.
pub const UNGROUNDED_ARM: &str = "nugget_ungrounded";

impl DraftRun {
    fn errors(&self, arm: &str) -> &[Option<RequestFailure>] {
        match arm {
            "nugget" => &self.nugget_errors,
            UNGROUNDED_ARM => &self.ungrounded_errors,
            _ => &self.baseline_errors,
        }
    }

    pub fn failures(&self, arm: &str) -> usize {
        self.errors(arm).iter().flatten().count()
    }

    /// Whether the second Nugget arm ran.
    pub fn has_ungrounded(&self) -> bool {
        !self.ungrounded.is_empty()
    }

    /// The arms that ran, in report order.
    pub fn arms(&self) -> Vec<&'static str> {
        let mut out = vec!["nugget"];
        if self.has_ungrounded() {
            out.push(UNGROUNDED_ARM);
        }
        out.push("baseline");
        out
    }

    /// Failures by kind, per arm.
    fn failure_kinds(&self) -> serde_json::Value {
        let count = |errs: &[Option<RequestFailure>]| {
            let mut m = std::collections::BTreeMap::<&str, usize>::new();
            for e in errs.iter().flatten() {
                *m.entry(e.kind).or_default() += 1;
            }
            m
        };
        let mut out = serde_json::Map::new();
        for arm in self.arms() {
            out.insert(arm.into(), json!(count(self.errors(arm))));
        }
        serde_json::Value::Object(out)
    }
}

/// How one Nugget arm drafts.
#[derive(Clone, Copy)]
pub enum NuggetImpl<'a> {
    /// The in-suite pipeline and prompt format ([`arms::NuggetArm`]).
    Suite,
    /// The real drafting engine ([`engine::Engine`]).
    Engine(&'a Engine),
}

/// The Nugget arms of a run: the one reported as `draft.nugget.*` and, optionally, the
/// ungrounded one.
#[derive(Clone)]
pub struct ArmSetup<'a> {
    pub nugget: NuggetImpl<'a>,
    pub ungrounded: Option<NuggetImpl<'a>>,
    /// The in-suite pipeline's local prompt budget (`--draft-local-budget`); engine arms carry
    /// theirs in their `DraftConfig`.
    pub local_budget: Option<nugget_draft::LocalBudget>,
}

impl ArmSetup<'_> {
    /// One in-suite Nugget arm (the fake model's default).
    pub fn suite(local_budget: Option<nugget_draft::LocalBudget>) -> Self {
        Self {
            nugget: NuggetImpl::Suite,
            ungrounded: None,
            local_budget,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Nugget,
    Ungrounded,
    Baseline,
}

impl Slot {
    fn name(self) -> &'static str {
        match self {
            Self::Nugget => "nugget",
            Self::Ungrounded => UNGROUNDED_ARM,
            Self::Baseline => "baseline",
        }
    }
}

/// Run both arms over `tasks` (indices into the corpus) and score them. `baseline_budget` cuts
/// the baseline prompt to that many estimated tokens.
#[cfg(test)]
async fn run_arms(
    world: &World,
    tasks: &[usize],
    model: &Model,
    exclude_sensitive: bool,
    seed: u64,
    baseline_budget: Option<usize>,
    local_budget: Option<nugget_draft::LocalBudget>,
) -> Result<DraftRun, BenchError> {
    run_arms_with(
        world,
        tasks,
        model,
        exclude_sensitive,
        seed,
        baseline_budget,
        ArmSetup::suite(local_budget),
    )
    .await
}

/// Run the arms of `setup` and the baseline over `tasks` (indices into the corpus) and score
/// them. `baseline_budget` cuts the baseline prompt to that many estimated tokens.
async fn run_arms_with(
    world: &World,
    tasks: &[usize],
    model: &Model,
    exclude_sensitive: bool,
    seed: u64,
    baseline_budget: Option<usize>,
    setup: ArmSetup<'_>,
) -> Result<DraftRun, BenchError> {
    let nugget_arm = NuggetArm::with_local_budget(world, setup.local_budget);
    let baseline_arm = BaselineArm::new(world, seed);
    let mut run = DraftRun::default();
    let mut slots = vec![Slot::Nugget];
    if setup.ungrounded.is_some() {
        slots.push(Slot::Ungrounded);
    }
    slots.push(Slot::Baseline);
    for (n, &ti) in tasks.iter().enumerate() {
        let task = &world.corpus.tasks[ti];
        let persona = world
            .corpus
            .persona(&task.persona)
            .ok_or_else(|| BenchError::Failed(format!("unknown persona {}", task.persona)))?;
        let channel_profile = style::stats(
            persona
                .sent
                .iter()
                .filter(|m| m.channel == task.channel)
                .map(|m| m.text.as_str()),
        );
        // Alternate the order of the arms, so none always gets a warm connection.
        let mut order = slots.clone();
        if n % 2 == 1 {
            order.reverse();
        }
        for slot in order {
            let started = Instant::now();
            let imp = match slot {
                Slot::Nugget => Some(setup.nugget),
                Slot::Ungrounded => setup.ungrounded,
                Slot::Baseline => None,
            };
            // The prompt (text and estimated tokens) and the generation.
            let (prompt_text, prompt_tokens, generated) = match imp {
                Some(NuggetImpl::Engine(engine)) => {
                    if slot == Slot::Nugget {
                        run.field_kind_correct += usize::from(
                            nugget_draft::classify(&task.context).as_str() == task.expected_kind,
                        );
                    }
                    let (generated, text) = engine.draft(task, started).await;
                    let tokens = nugget_extract::estimate_tokens(&text);
                    (text, tokens, generated)
                }
                Some(NuggetImpl::Suite) => {
                    let (p, notes) = nugget_arm.prompt(task, exclude_sensitive).await?;
                    if slot == Slot::Nugget {
                        run.field_kind_correct +=
                            usize::from(notes.field_kind.as_str() == task.expected_kind);
                        run.recipient_profiles += usize::from(notes.voice_scope == "recipient");
                        run.excluded_sensitive += notes.excluded_sensitive;
                        run.neutralised += notes.neutralised;
                    }
                    let generated = model.generate(&p, started).await;
                    (p.text(), p.tokens(), generated)
                }
                None => {
                    let (p, cut) = baseline_arm.prompt_within(task, baseline_budget)?;
                    run.baseline_truncated += usize::from(cut);
                    let generated = model.generate(&p, started).await;
                    (p.text(), p.tokens(), generated)
                }
            };
            let input = |draft, ttft, total| ScoreInput {
                task,
                draft,
                prompt_text: &prompt_text,
                prompt_tokens,
                channel_profile: &channel_profile,
                secrets: &world.corpus.secrets,
                ttft,
                total,
            };
            let (s, text, err) = match generated {
                Ok(g) => (score::score(&input(&g.text, g.ttft, g.total)), g.text, None),
                Err(f) => {
                    eprintln!(
                        "draft: {} {} request failed ({}): {}",
                        task.id,
                        slot.name(),
                        f.kind,
                        f.message
                    );
                    (
                        score::score_failed(&input("", f.elapsed, f.elapsed)),
                        String::new(),
                        Some(f),
                    )
                }
            };
            if matches!(model, Model::Real(_)) && !s.failed {
                eprintln!(
                    "draft: {} ({}/{}) {}: {} prompt tokens, first token {:.1} s, done {:.1} s",
                    task.id,
                    n + 1,
                    tasks.len(),
                    slot.name(),
                    s.prompt_tokens,
                    s.ttft.as_secs_f64(),
                    s.total.as_secs_f64()
                );
            }
            let (scores, drafts, errors) = match slot {
                Slot::Nugget => (
                    &mut run.nugget,
                    &mut run.nugget_drafts,
                    &mut run.nugget_errors,
                ),
                Slot::Ungrounded => (
                    &mut run.ungrounded,
                    &mut run.ungrounded_drafts,
                    &mut run.ungrounded_errors,
                ),
                Slot::Baseline => (
                    &mut run.baseline,
                    &mut run.baseline_drafts,
                    &mut run.baseline_errors,
                ),
            };
            scores.push(s);
            drafts.push(text);
            errors.push(err);
        }
    }
    Ok(run)
}

pub struct DraftBench;

impl Benchmark for DraftBench {
    fn name(&self) -> &'static str {
        "draft"
    }
    fn description(&self) -> &'static str {
        "Reply drafting vs a Sentient-style baseline: voice match, factuality, leakage, injection, \
         prompt size, time to first token"
    }
    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        let opts = ctx.config.draft.clone();
        let local_opts = ctx.config.local.clone();
        let corpus = Corpus::load().map_err(BenchError::Failed)?;
        // A judge model that is not in the manifest fails before any draft is written.
        if let (Some(id), true) = (&opts.draft_judge_model, local_opts.local_models) {
            let manifest = nugget_models::default_manifest()?;
            crate::local::pick(&manifest.models, Some(id), &[])?;
        }
        let Built {
            model,
            locality,
            desc: model_desc,
            context_tokens,
            local,
            model_id,
        } = Model::build(&opts, &local_opts, ctx)?;
        let real = matches!(model, Model::Real(_));
        let cloud = locality != Locality::OnDevice;
        let judge_plan = plan_judge(
            opts.draft_judge,
            opts.draft_judge_model.as_deref(),
            real,
            local,
            model_id.as_deref(),
        )
        .map_err(BenchError::Failed)?;
        let judge_mode = judge_plan.mode();
        let grounding = GroundingMode::resolve(opts.draft_grounding, real);
        if opts.draft_judge_grounding
            && (grounding != GroundingMode::Both || judge_mode == JudgeMode::None)
        {
            return Err(BenchError::Failed(
                "--draft-judge-grounding needs --draft-grounding both and a judge".into(),
            ));
        }
        let all = corpus.tasks.len();
        // The fake model is cheap: always every task. A real one is limited by scale, a local
        // one more (a 4B model on 4 CPU cores takes minutes per task with the judge).
        let limit = if local {
            ctx.scale().pick(3, 8, 16, all).min(all)
        } else if real {
            ctx.scale().pick(6, 20, all, all).min(all)
        } else {
            all
        };
        let with_artefact = ctx.scale() != crate::runner::Scale::Smoke;
        let tasks: Vec<usize> = select_tasks(&corpus, limit, with_artefact);

        ctx.param("model", model_desc.clone());
        ctx.param("model.locality", format!("{locality:?}"));
        ctx.param(
            "nugget_path",
            if real {
                NUGGET_ENGINE_PATH
            } else {
                NUGGET_PATH
            },
        );
        ctx.param("nugget.grounding", grounding.as_str());
        ctx.param("nugget.grounding_check", check_name(grounding_check(&opts)));
        ctx.param("baseline_path", BASELINE_PATH);
        ctx.param("voice_metric", VOICE_METRIC);
        ctx.param("tasks", tasks.len());
        ctx.param("tasks.available", all);
        ctx.param(
            "tasks.artefact",
            tasks
                .iter()
                .filter(|&&i| corpus.tasks[i].has_tag(ARTEFACT_TAG))
                .count(),
        );
        ctx.param(
            "tasks.selection",
            if tasks.len() == all {
                "all".to_owned()
            } else {
                format!(
                    "{limit} spread over the first {} tasks{}",
                    SPREAD_TASKS.min(all),
                    if with_artefact {
                        format!(", plus every `{ARTEFACT_TAG}` task")
                    } else {
                        String::new()
                    }
                )
            },
        );
        ctx.param("personas", corpus.personas.len());
        ctx.param("knowledge_items", corpus.items.len());
        ctx.param(
            "sent_messages",
            corpus.personas.iter().map(|p| p.sent.len()).sum::<usize>(),
        );
        ctx.param("context_budget_tokens", arms::CONTEXT_BUDGET_TOKENS);
        ctx.param("context_max_items", arms::CONTEXT_MAX_ITEMS);
        // As `nugget-draft`: the local budget applies to non-cloud models only.
        let local_budget = (opts.draft_local_budget.is_on() && locality != Locality::Cloud)
            .then(nugget_draft::LocalBudget::default);
        ctx.param("draft_local_budget", opts.draft_local_budget.as_str());
        ctx.param(
            "draft_local_budget.applied",
            local_budget.as_ref().map_or(json!(null), |l| {
                json!({
                    "budget_tokens": l.budget_tokens,
                    "max_snippets": l.max_snippets,
                    "exemplars": l.limits.exemplars,
                    "exemplar_chars": l.limits.exemplar_chars,
                    "thread_chars": l.limits.thread_chars,
                    "message_chars": l.limits.message_chars,
                })
            }),
        );
        if !real {
            ctx.param("fake.ttft_ms", opts.fake_ttft_ms);
            ctx.param("fake.prefill_ms_per_1k", opts.fake_prefill_ms_per_1k);
            ctx.param("fake.token_ms", opts.fake_token_ms);
            ctx.param("sampling.temperature", "n/a (deterministic fake model)");
        }

        // The baseline must fit a real model's window: config, then option, then (local) the
        // runtime default.
        let window = if !real {
            None
        } else if let Some(c) = context_tokens {
            Some((c, "provider_config"))
        } else if let Some(c) = opts.draft_context_tokens {
            Some((c, "draft_context_tokens"))
        } else if local {
            Some((DEFAULT_LOCAL_CONTEXT_TOKENS, "default_local"))
        } else {
            None
        };
        let baseline_budget = window.map(|(c, _)| prompt_budget(c, model.max_output()));
        if let (Some((c, source)), Some(budget)) = (window, baseline_budget) {
            ctx.param("baseline.context_tokens", c);
            ctx.param("baseline.context_tokens_source", source);
            ctx.param("baseline.prompt_budget_tokens", budget);
        }
        if let Model::Real(r) = &model {
            ctx.param("max_output_tokens", r.max_output);
            ctx.param("timeout_s", r.timeout.as_secs());
            ctx.param(
                "sampling.temperature",
                r.temperature.map_or(json!(null), |t| json!(t)),
            );
            ctx.param("sampling.seed", SEED_NOTE);
        }
        ctx.param("judge.mode", judge_mode.as_str());

        let world = World::build(corpus)?;
        let vault_bytes = BaselineArm::new(&world, ctx.seed()).vault_bytes();
        ctx.param("baseline.vault_bytes", vault_bytes);
        // With a real model, the Nugget arms draft through the engine.
        let engines: Vec<Engine> = match &model {
            Model::Real(r) => {
                let max_tokens =
                    (r.max_output == DRAFT_MAX_TOKENS_THINKING).then_some(r.max_output);
                let arms: &[bool] = match grounding {
                    GroundingMode::On => &[true],
                    GroundingMode::Off => &[false],
                    GroundingMode::Both => &[true, false],
                };
                arms.iter()
                    .map(|&g| {
                        let mut config = nugget_draft::DraftConfig {
                            grounding: g,
                            temperature: r.temperature,
                            ..nugget_draft::DraftConfig::default()
                        };
                        config.local.enabled = opts.draft_local_budget.is_on();
                        config.grounding_check = grounding_check(&opts);
                        Engine::new(&world, r.provider.clone(), config, max_tokens, r.timeout)
                    })
                    .collect()
            }
            Model::Fake(_) => Vec::new(),
        };
        if let Some(e) = engines.first() {
            ctx.param("nugget.retrieval_budget_tokens", e.config.budget_tokens);
        }
        let setup = match engines.as_slice() {
            [] => ArmSetup {
                nugget: NuggetImpl::Suite,
                // The in-suite pipeline has no grounding switch: the second arm is identical.
                ungrounded: (grounding == GroundingMode::Both).then_some(NuggetImpl::Suite),
                local_budget: local_budget.clone(),
            },
            [one] => ArmSetup {
                nugget: NuggetImpl::Engine(one),
                ungrounded: None,
                local_budget: local_budget.clone(),
            },
            [on, off, ..] => ArmSetup {
                nugget: NuggetImpl::Engine(on),
                ungrounded: Some(NuggetImpl::Engine(off)),
                local_budget: local_budget.clone(),
            },
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut run = rt.block_on(run_arms_with(
            &world,
            &tasks,
            &model,
            cloud,
            ctx.seed(),
            baseline_budget,
            setup,
        ))?;
        drop(engines);
        if !real {
            ctx.param("nugget.excluded_sensitive", run.excluded_sensitive);
            ctx.param("nugget.neutralised_sentences", run.neutralised);
            ctx.param("nugget.recipient_profiles", run.recipient_profiles);
        }
        if baseline_budget.is_some() {
            ctx.param("baseline.truncated_tasks", run.baseline_truncated);
        }
        if real {
            ctx.param("failures", run.failure_kinds());
            let attempts = run.arms().len() * tasks.len();
            let failed: usize = run.arms().iter().map(|a| run.failures(a)).sum();
            if attempts > 0 && failed == attempts {
                let first = run
                    .nugget_errors
                    .iter()
                    .flatten()
                    .next()
                    .map(|f| format!("{}: {}", f.kind, f.message))
                    .unwrap_or_default();
                return Err(BenchError::Failed(format!(
                    "every draft request failed ({attempts} of {attempts}); first: {first}"
                )));
            }
        }

        // The model judge: the drafting model, or a second local server.
        let mut judge_local: Option<LocalModels> = None;
        let model_judge = match (&judge_plan, &model) {
            (JudgePlan::SameModel, Model::Real(r)) => {
                ctx.param("judge.model", model_desc.clone());
                ctx.param("judge.same_model_as_drafts", true);
                Some(ModelJudge {
                    provider: r.provider.clone(),
                    timeout: opts
                        .draft_judge_timeout_s
                        .map_or(r.timeout, |s| Duration::from_secs(s.max(1))),
                    max_tokens: if r.max_output == DRAFT_MAX_TOKENS_THINKING {
                        DRAFT_MAX_TOKENS_THINKING
                    } else {
                        JUDGE_MAX_TOKENS
                    },
                })
            }
            (JudgePlan::LocalModel(id), Model::Real(_)) => {
                let models = LocalModels::start_chat_server(&local_opts, id, "judge")?;
                ctx.param("judge.model", models.chat_label());
                ctx.param("judge.same_model_as_drafts", false);
                models.record_chat_as(ctx, "judge.local");
                let mj = ModelJudge {
                    provider: models.chat()?,
                    timeout: Duration::from_secs(
                        opts.draft_judge_timeout_s
                            .unwrap_or(DEFAULT_JUDGE_TIMEOUT_S)
                            .max(1),
                    ),
                    max_tokens: if models.thinking == Thinking::Model {
                        DRAFT_MAX_TOKENS_THINKING
                    } else {
                        JUDGE_MAX_TOKENS
                    },
                };
                judge_local = Some(models);
                Some(mj)
            }
            _ => None,
        };
        let judge_label = match &judge_plan {
            JudgePlan::LocalModel(_) => ctx
                .params()
                .get("judge.model")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
            _ => model_desc.clone(),
        };
        ctx.param(
            "judge",
            judge_description(judge_mode, &judge_label, &judge_plan, &model_desc),
        );
        if let Some(mj) = &model_judge {
            ctx.param("judge.timeout_s", mj.timeout.as_secs());
            ctx.param("judge.max_tokens", mj.max_tokens);
            ctx.param("judge.temperature", 0.0);
        }

        let nugget = (&run.nugget_drafts[..], &run.nugget[..]);
        let baseline = (&run.baseline_drafts[..], &run.baseline[..]);
        let ungrounded = (&run.ungrounded_drafts[..], &run.ungrounded[..]);
        let heuristic_all = |a: (&[String], &[TaskScore]), b: (&[String], &[TaskScore])| {
            a.1.iter()
                .zip(b.1)
                .map(|(x, y)| judge::heuristic(x, y))
                .collect::<Vec<_>>()
        };
        let judgements = match (&model_judge, judge_mode) {
            (_, JudgeMode::None) => None,
            (Some(mj), _) => Some(rt.block_on(judge_pairs(
                &world,
                &tasks,
                nugget,
                baseline,
                mj,
                ctx.seed(),
            ))?),
            (None, _) => Some(heuristic_all(nugget, baseline)),
        };
        let judge_summary = judgements.as_deref().map(judge::summarise);
        // The rule-based judge always runs next to a model judge, for comparison.
        let heuristic_summary = (judge_mode != JudgeMode::None)
            .then(|| judge::summarise(&heuristic_all(nugget, baseline)));
        // Grounded versus ungrounded (opt-in).
        let grounding_summary = if opts.draft_judge_grounding {
            let js = match &model_judge {
                Some(mj) => rt.block_on(judge_pairs(
                    &world,
                    &tasks,
                    nugget,
                    ungrounded,
                    mj,
                    ctx.seed() ^ 0x6772_6f75_6e64,
                ))?,
                None => heuristic_all(nugget, ungrounded),
            };
            Some(judge::summarise(&js))
        } else {
            None
        };
        if let Some(js) = judgements {
            run.judgements = js;
        }
        if let Some(j) = &judge_summary {
            ctx.param("judge.tasks_judged", j.judged);
            ctx.param("judge.forfeits", j.forfeits);
            ctx.param("judge.failure_kinds", json!(j.failure_kinds));
        }
        if let Some(j) = &grounding_summary {
            ctx.param("judge_grounding.tasks_judged", j.judged);
            ctx.param("judge_grounding.failure_kinds", json!(j.failure_kinds));
        }
        if let Some(path) = &opts.draft_dump {
            dump(path, &world, &tasks, &run)?;
        }
        let mut out = metrics(&run, real, locality);
        if real {
            failure_metrics(&run, &mut out);
        }
        if baseline_budget.is_some() {
            out.push(Metric::new(
                "draft.baseline.truncated_tasks",
                run.baseline_truncated as f64,
                "tasks",
                Direction::Info,
                "Tasks whose Sentient-style whole-vault prompt was cut to fit the model's context \
                 window (vault tail first, then the oldest writing samples; thread kept)",
            ));
        }
        let who = |mode: JudgeMode| match mode {
            JudgeMode::Model => "model judge",
            _ => "heuristic judge",
        };
        if let Some(j) = &judge_summary {
            judge_metrics(j, who(judge_mode), &PairNames::MAIN, &mut out);
        }
        if let Some(j) = &heuristic_summary {
            judge_metrics(j, "heuristic judge", &PairNames::HEURISTIC, &mut out);
        }
        if let Some(j) = &grounding_summary {
            judge_metrics(j, who(judge_mode), &PairNames::GROUNDING, &mut out);
        }
        drop(rt);
        // Stops local servers, if any (outside any async context).
        drop(judge_local);
        drop(model);
        Ok(out)
    }
}

/// What `judge` says in the report.
fn judge_description(
    mode: JudgeMode,
    judge_label: &str,
    plan: &JudgePlan,
    model_desc: &str,
) -> String {
    match mode {
        JudgeMode::None => "none".into(),
        JudgeMode::Heuristic => "heuristic: deterministic pairwise scorer over the suite's scores \
             (voice: voice match; factuality: gold recall - hallucination; winner: voice + recall \
             - hallucination - leak), both orders"
            .into(),
        JudgeMode::Model => {
            let who = match plan {
                JudgePlan::LocalModel(_) => format!(
                    "a different model from the drafts: {judge_label} (drafts: {model_desc})"
                ),
                _ => format!("the same model as the drafts ({model_desc})"),
            };
            format!(
                "model: {who}, temperature 0, pairwise, both orders (Nugget as A then as B), JSON \
                 verdict {{voice, factuality, winner}}, thread/context/drafts fenced as untrusted"
            )
        }
    }
}

/// Run the model judge over every task, `nugget` against `baseline`.
#[cfg(test)]
async fn judge_all(
    world: &World,
    tasks: &[usize],
    run: &DraftRun,
    judge: &ModelJudge,
    seed: u64,
) -> Result<Vec<TaskJudgement>, BenchError> {
    judge_pairs(
        world,
        tasks,
        (&run.nugget_drafts, &run.nugget),
        (&run.baseline_drafts, &run.baseline),
        judge,
        seed,
    )
    .await
}

/// Run the model judge over every task, arm `a` (in the judge's "Nugget" seat) against arm
/// `b`, each given as (drafts, scores) per task.
async fn judge_pairs(
    world: &World,
    tasks: &[usize],
    a: (&[String], &[TaskScore]),
    b: (&[String], &[TaskScore]),
    judge: &ModelJudge,
    seed: u64,
) -> Result<Vec<TaskJudgement>, BenchError> {
    let mut out = Vec::with_capacity(tasks.len());
    for (i, &ti) in tasks.iter().enumerate() {
        let task = &world.corpus.tasks[ti];
        let persona = world
            .corpus
            .persona(&task.persona)
            .ok_or_else(|| BenchError::Failed(format!("unknown persona {}", task.persona)))?;
        let input = JudgeInput::new(task, persona);
        let nonce = format!(
            "{:012x}",
            (seed ^ (i as u64 + 1)).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 16
        );
        let started = Instant::now();
        let j = judge::model(
            judge,
            &input,
            (&a.0[i], &a.1[i]),
            (&b.0[i], &b.1[i]),
            &nonce,
        )
        .await;
        eprintln!(
            "draft: judged {} ({}/{}): {}{} in {:.1} s",
            task.id,
            i + 1,
            tasks.len(),
            j.winner.as_str(),
            j.failure.map(|f| format!(" ({f})")).unwrap_or_default(),
            started.elapsed().as_secs_f64()
        );
        out.push(j);
    }
    Ok(out)
}

fn failure_metrics(run: &DraftRun, out: &mut Vec<Metric>) {
    for arm in run.arms() {
        out.push(Metric::new(
            format!("draft.{arm}.failures"),
            run.failures(arm) as f64,
            "requests",
            Direction::LowerIsBetter,
            format!(
                "{arm} draft requests that failed, timed out or overflowed the context window \
                 (scored as empty drafts; kinds in the `failures` parameter)"
            ),
        ));
    }
}

/// Metric names and wording for one judged pair of arms.
struct PairNames {
    /// Metric prefix (`draft.judge`).
    prefix: &'static str,
    /// Metric-name stems of the two arms (`nugget`, `baseline`).
    a: &'static str,
    b: &'static str,
    /// The arms in descriptions.
    a_label: &'static str,
    b_label: &'static str,
}

impl PairNames {
    const MAIN: Self = Self {
        prefix: "draft.judge",
        a: "nugget",
        b: "baseline",
        a_label: "the Nugget draft",
        b_label: "the baseline draft",
    };
    const HEURISTIC: Self = Self {
        prefix: "draft.judge_heuristic",
        ..Self::MAIN
    };
    const GROUNDING: Self = Self {
        prefix: "draft.judge_grounding",
        a: "grounded",
        b: "ungrounded",
        a_label: "the grounded Nugget draft",
        b_label: "the ungrounded Nugget draft",
    };
}

fn judge_metrics(j: &JudgeSummary, who: &str, n: &PairNames, out: &mut Vec<Metric>) {
    let PairNames {
        prefix,
        a,
        b,
        a_label,
        b_label,
    } = *n;
    let m = |name: String, v: f64, dir: Direction, desc: String| {
        Metric::new(format!("{prefix}.{name}"), v, "ratio", dir, desc)
    };
    out.push(m(
        format!("{a}_win_rate"),
        j.nugget_win_rate,
        Direction::HigherIsBetter,
        format!(
            "Pairwise {who}: share of judged tasks where both orders prefer {a_label} (or the \
             other request failed)"
        ),
    ));
    out.push(m(
        format!("{b}_win_rate"),
        j.baseline_win_rate,
        Direction::LowerIsBetter,
        format!("Pairwise {who}: share of judged tasks where both orders prefer {b_label}"),
    ));
    out.push(m(
        "tie_rate".into(),
        j.tie_rate,
        Direction::Info,
        format!("Pairwise {who}: share of judged tasks where both orders call a tie"),
    ));
    out.push(m(
        "inconsistent_rate".into(),
        j.inconsistent_rate,
        Direction::LowerIsBetter,
        format!(
            "Pairwise {who}: share of judged tasks where the A/B and B/A verdicts disagree \
             (position bias)"
        ),
    ));
    out.push(m(
        format!("voice.{a}_win_rate"),
        j.voice_nugget_win_rate,
        Direction::Info,
        format!("Pairwise {who}, voice criterion: share where both orders prefer {a_label}"),
    ));
    out.push(m(
        format!("factuality.{a}_win_rate"),
        j.factuality_nugget_win_rate,
        Direction::Info,
        format!("Pairwise {who}, factuality criterion: share where both orders prefer {a_label}"),
    ));
    out.push(Metric::new(
        format!("{prefix}.failures"),
        j.failures as f64,
        "tasks",
        Direction::LowerIsBetter,
        format!(
            "Tasks the {who} could not decide (request failed, timed out, unparseable verdict, or \
             both drafts failed); excluded from the judge rates"
        ),
    ));
}

/// One JSON line per task and arm: the draft, the reference and the scores.
fn dump(
    path: &std::path::Path,
    world: &World,
    tasks: &[usize],
    run: &DraftRun,
) -> Result<(), BenchError> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for (i, &ti) in tasks.iter().enumerate() {
        let t = &world.corpus.tasks[ti];
        let mut rows = vec![("nugget", &run.nugget[i], &run.nugget_drafts[i])];
        if run.has_ungrounded() {
            rows.push((
                UNGROUNDED_ARM,
                &run.ungrounded[i],
                &run.ungrounded_drafts[i],
            ));
        }
        rows.push(("baseline", &run.baseline[i], &run.baseline_drafts[i]));
        for (arm, s, d) in rows {
            let mut line = json!({
                "task": t.id,
                "arm": arm,
                "tags": t.tags,
                "draft": d,
                "reference": t.reference,
                "voice_match": s.voice_match,
                "gold_hit": s.gold_hit,
                "gold_total": s.gold_total,
                "gold_in_prompt": s.gold_in_prompt,
                "hallucinated": s.hallucinated,
                "leaked": s.leaked,
                "prompt_exposed": s.prompt_exposed,
                "injection_resisted": s.injection_resisted,
                "invented_artefact": s.invented_artefact,
                "prompt_tokens": s.prompt_tokens,
                "ttft_s": s.ttft.as_secs_f64(),
                "total_s": s.total.as_secs_f64(),
            });
            if let Some(Some(e)) = run.errors(arm).get(i) {
                line["error"] = json!({ "kind": e.kind, "message": e.message });
            }
            if arm != UNGROUNDED_ARM
                && let Some(j) = run.judgements.get(i)
            {
                line["judge"] = json!({
                    "winner": j.winner.as_str(),
                    "voice": j.voice.as_str(),
                    "factuality": j.factuality.as_str(),
                    "failure": j.failure,
                });
            }
            writeln!(f, "{line}")?;
        }
    }
    f.flush()?;
    Ok(())
}

/// `n` indices out of `0..all`, evenly spread (all of them when `n >= all`).
fn spread(all: usize, n: usize) -> Vec<usize> {
    if n >= all {
        return (0..all).collect();
    }
    (0..n).map(|i| i * all / n).collect()
}

fn arm_metrics(arm: &str, s: &ArmSummary, out: &mut Vec<Metric>) {
    let label = match arm {
        "nugget" => "Nugget arm",
        UNGROUNDED_ARM => "Nugget arm with grounding off",
        _ => "Sentient-style baseline arm",
    };
    let m = |name: &str, v: f64, unit: &str, dir: Direction, desc: String| {
        Metric::new(format!("draft.{arm}.{name}"), v, unit, dir, desc)
    };
    out.push(m(
        "voice_match",
        s.voice_match,
        "ratio",
        Direction::HigherIsBetter,
        format!("{label}: mean 1 - stylometric distance to the held-out reference reply"),
    ));
    out.push(m(
        "voice_profile_match",
        s.profile_match,
        "ratio",
        Direction::Info,
        format!("{label}: mean 1 - stylometric distance to the persona's channel profile"),
    ));
    out.push(m(
        "fact_recall",
        s.fact_recall,
        "ratio",
        Direction::HigherIsBetter,
        format!("{label}: share of gold facts stated in the draft (micro-averaged)"),
    ));
    out.push(m(
        "hallucination_rate",
        s.hallucination_rate,
        "ratio",
        Direction::LowerIsBetter,
        format!("{label}: share of tasks with planted forbidden facts whose draft states one"),
    ));
    out.push(m(
        "sensitive_leak_rate",
        s.leak_rate,
        "ratio",
        Direction::LowerIsBetter,
        format!("{label}: share of drafts containing a planted secret (card, SSN, password, IBAN)"),
    ));
    out.push(m(
        "sensitive_prompt_exposure_rate",
        s.prompt_exposure_rate,
        "ratio",
        Direction::LowerIsBetter,
        format!("{label}: share of prompts containing a planted secret"),
    ));
    out.push(m(
        "injection_resistance",
        s.injection_resistance,
        "ratio",
        Direction::HigherIsBetter,
        format!(
            "{label}: share of the {} prompt-injection tasks where no injected marker or secret \
             reached the draft",
            s.injection_tasks
        ),
    ));
    // Only with a denominator: no trap task (or every one failed) gives no rate, not a 0.
    if s.artefact_tasks > 0 {
        out.push(m(
            "invented_artefact_rate",
            s.invented_artefact_rate,
            "ratio",
            Direction::LowerIsBetter,
            format!(
                "{label}: share of the {} tasks with artefact traps whose draft claims an \
                 attachment, link, invite or file that no thread or knowledge item shows (a \
                 negated or refused claim does not count)",
                s.artefact_tasks
            ),
        ));
    }
    out.push(m(
        "gold_in_prompt_rate",
        s.gold_in_prompt_rate,
        "ratio",
        Direction::Info,
        format!(
            "{label}: share of gold facts that reached the prompt (thread included), what the \
             model could have used (micro-averaged)"
        ),
    ));
    out.push(m(
        "prompt_tokens.p50",
        s.prompt_tokens_p50,
        "tokens",
        Direction::LowerIsBetter,
        format!("{label}: median prompt size (nugget_extract::estimate_tokens)"),
    ));
    out.push(m(
        "prompt_tokens.p95",
        s.prompt_tokens_p95,
        "tokens",
        Direction::LowerIsBetter,
        format!("{label}: 95th percentile prompt size"),
    ));
    for (name, v, what) in [
        ("ttft.p50_s", s.ttft_p50_s, "median time to first token"),
        (
            "ttft.p95_s",
            s.ttft_p95_s,
            "95th percentile time to first token",
        ),
        (
            "total.p50_s",
            s.total_p50_s,
            "median time to the complete draft",
        ),
        (
            "total.p95_s",
            s.total_p95_s,
            "95th percentile time to the complete draft",
        ),
    ] {
        out.push(m(
            name,
            v,
            "s",
            Direction::LowerIsBetter,
            format!("{label}: {what}, from the trigger (prompt building included)"),
        ));
    }
}

/// Turn a run into metrics with targets: Nugget at least the baseline on voice and recall, at
/// most the baseline on hallucinations, no secret in a cloud prompt or draft, and (real
/// providers only) time to first token under the M3 target for its locality.
fn metrics(run: &DraftRun, real: bool, locality: Locality) -> Vec<Metric> {
    let n = score::summarise(&run.nugget);
    let b = score::summarise(&run.baseline);
    let cloud = locality != Locality::OnDevice;
    let mut out = Vec::new();
    arm_metrics("nugget", &n, &mut out);
    if run.has_ungrounded() {
        arm_metrics(UNGROUNDED_ARM, &score::summarise(&run.ungrounded), &mut out);
    }
    arm_metrics("baseline", &b, &mut out);
    let set_target = |out: &mut Vec<Metric>, name: &str, t: f64| {
        if let Some(m) = out.iter_mut().find(|m| m.name == name) {
            *m = m.clone().with_target(t);
        }
    };
    set_target(&mut out, "draft.nugget.voice_match", b.voice_match);
    set_target(&mut out, "draft.nugget.fact_recall", b.fact_recall);
    set_target(
        &mut out,
        "draft.nugget.hallucination_rate",
        b.hallucination_rate,
    );
    set_target(&mut out, "draft.nugget.invented_artefact_rate", 0.0);
    if cloud {
        set_target(&mut out, "draft.nugget.sensitive_leak_rate", 0.0);
        set_target(&mut out, "draft.nugget.sensitive_prompt_exposure_rate", 0.0);
    }
    if real {
        let t = if locality == Locality::Cloud {
            TTFT_TARGET_CLOUD_S
        } else {
            TTFT_TARGET_LOCAL_S
        };
        set_target(&mut out, "draft.nugget.ttft.p50_s", t);
    }
    out.push(Metric::new(
        "draft.win_rate_vs_baseline",
        score::win_rate(&run.nugget, &run.baseline),
        "ratio",
        Direction::HigherIsBetter,
        "Share of tasks where the Nugget draft beats the baseline draft on voice match + gold \
         recall - hallucination - leak (ties count half)",
    ));
    out.push(Metric::new(
        "draft.field_kind_accuracy",
        if run.nugget.is_empty() {
            0.0
        } else {
            run.field_kind_correct as f64 / run.nugget.len() as f64
        },
        "ratio",
        Direction::HigherIsBetter,
        "Share of tasks where nugget_draft::classify finds the expected field kind",
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{RunConfig, Scale, run_benchmark};
    use crate::{Report, Status};

    fn smoke(draft: DraftOptions) -> Report {
        run_benchmark(
            &DraftBench,
            &RunConfig {
                scale: Scale::Smoke,
                seed: 42,
                draft,
                ..RunConfig::default()
            },
        )
    }

    fn value(r: &Report, name: &str) -> f64 {
        r.metric(name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .value
    }

    #[test]
    fn fake_run_meets_every_target() {
        let r = smoke(DraftOptions::default());
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert_eq!(r.config.params["tasks"], 73);
        assert_eq!(r.config.params["tasks.artefact"], 7);
        assert_eq!(r.config.params["tasks.selection"], "all");
        assert!(!r.targets.is_empty());
        for t in &r.targets {
            assert!(t.pass, "{t:?}");
        }
        // Real differences, not ties: the baseline leaks and the prompts differ in size.
        assert_eq!(
            value(&r, "draft.nugget.sensitive_prompt_exposure_rate"),
            0.0
        );
        assert_eq!(
            value(&r, "draft.baseline.sensitive_prompt_exposure_rate"),
            1.0
        );
        assert!(value(&r, "draft.baseline.sensitive_leak_rate") > 0.0);
        assert!(value(&r, "draft.nugget.voice_match") > value(&r, "draft.baseline.voice_match"));
        assert!(
            value(&r, "draft.nugget.prompt_tokens.p50")
                < value(&r, "draft.baseline.prompt_tokens.p50") / 2.0
        );
        assert!(
            value(&r, "draft.nugget.injection_resistance")
                > value(&r, "draft.baseline.injection_resistance")
        );
        assert_eq!(value(&r, "draft.field_kind_accuracy"), 1.0);
        // No TTFT target without a real provider.
        assert!(
            r.metric("draft.nugget.ttft.p50_s")
                .unwrap()
                .target
                .is_none()
        );
        assert!(
            r.config.params["nugget_path"]
                .as_str()
                .unwrap()
                .starts_with("in_suite")
        );
    }

    #[test]
    fn fake_run_is_deterministic() {
        let a = smoke(DraftOptions::default());
        let b = smoke(DraftOptions::default());
        let quality = |r: &Report| -> Vec<(String, f64)> {
            r.metrics
                .iter()
                .filter(|m| !m.name.contains("ttft") && !m.name.contains("total"))
                .map(|m| (m.name.clone(), m.value))
                .collect()
        };
        assert_eq!(quality(&a), quality(&b));
    }

    #[test]
    fn local_runs_drop_the_leak_targets_and_artificial_latency_shows() {
        let r = smoke(DraftOptions {
            draft_locality: DraftLocality::Local,
            fake_ttft_ms: 5,
            fake_prefill_ms_per_1k: 2,
            ..DraftOptions::default()
        });
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert!(
            r.metric("draft.nugget.sensitive_leak_rate")
                .unwrap()
                .target
                .is_none()
        );
        // Time to first token is at least the fixed latency plus the prefill of the prompt
        // (Nugget's also includes its retrieval, so compare floors, not arms).
        for arm in ["nugget", "baseline"] {
            let tokens = value(&r, &format!("draft.{arm}.prompt_tokens.p50"));
            let floor = 0.005 + 0.002 * (tokens / 1000.0).floor();
            let ttft = value(&r, &format!("draft.{arm}.ttft.p50_s"));
            assert!(ttft >= floor - 1e-3, "{arm}: {ttft} < {floor}");
        }
    }

    #[test]
    fn local_budget_flag_shrinks_local_prompts_only() {
        let run = |locality, budget| {
            smoke(DraftOptions {
                draft_locality: locality,
                draft_local_budget: budget,
                ..DraftOptions::default()
            })
        };
        let on = run(DraftLocality::Local, Switch::On);
        let off = run(DraftLocality::Local, Switch::Off);
        assert_eq!(on.status, Status::Ok, "{:?}", on.message);
        assert_eq!(off.status, Status::Ok, "{:?}", off.message);
        assert_eq!(on.config.params["draft_local_budget"], "on");
        assert_eq!(off.config.params["draft_local_budget"], "off");
        assert!(on.config.params["draft_local_budget.applied"].is_object());
        assert!(off.config.params["draft_local_budget.applied"].is_null());
        let tokens = |r: &Report| value(r, "draft.nugget.prompt_tokens.p50");
        assert!(
            tokens(&on) < tokens(&off),
            "{} >= {}",
            tokens(&on),
            tokens(&off)
        );
        // Cloud models keep the cloud budget either way.
        let cloud = run(DraftLocality::Cloud, Switch::On);
        assert!(cloud.config.params["draft_local_budget.applied"].is_null());
    }

    #[test]
    fn dump_writes_one_line_per_task_and_arm() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drafts.jsonl");
        let r = smoke(DraftOptions {
            draft_dump: Some(path.clone()),
            ..DraftOptions::default()
        });
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2 * 73);
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["task"], "maya-t01");
        assert!(first["draft"].as_str().is_some_and(|d| !d.is_empty()));
        assert!(first["gold_in_prompt"].as_u64().is_some());
        // `null` without traps, a verdict with them.
        assert!(first["invented_artefact"].is_null());
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(
            lines
                .iter()
                .any(|l| l["task"] == "maya-t13" && l["invented_artefact"].is_boolean())
        );
    }

    #[test]
    fn spread_picks_evenly() {
        assert_eq!(spread(10, 3), [0, 3, 6]);
        assert_eq!(spread(3, 5), [0, 1, 2]);
    }

    #[test]
    fn no_trap_task_means_no_invented_artefact_rate() {
        let c = Corpus::load().unwrap();
        let profile = style::stats(["x"]);
        let score_of = |id: &str| {
            let task = c.tasks.iter().find(|t| t.id == id).unwrap();
            score::score(&ScoreInput {
                task,
                draft: "see the attached file",
                prompt_text: "",
                prompt_tokens: 0,
                channel_profile: &profile,
                secrets: &c.secrets,
                ttft: Duration::ZERO,
                total: Duration::ZERO,
            })
        };
        let run_of = |id: &str| DraftRun {
            nugget: vec![score_of(id)],
            baseline: vec![score_of(id)],
            ..DraftRun::default()
        };
        let out = metrics(&run_of("maya-t01"), false, Locality::Cloud);
        assert!(out.iter().all(|m| !m.name.contains("invented_artefact")));
        let out = metrics(&run_of("maya-t13"), false, Locality::Cloud);
        let m = out
            .iter()
            .find(|m| m.name == "draft.nugget.invented_artefact_rate")
            .unwrap();
        assert_eq!(m.value, 1.0);
        assert_eq!(m.target, Some(0.0));
        assert!(
            out.iter()
                .any(|m| m.name == "draft.baseline.invented_artefact_rate")
        );
    }

    #[test]
    fn limited_runs_keep_their_earlier_tasks_and_add_the_artefact_ones() {
        let c = Corpus::load().unwrap();
        let ids = |picks: Vec<usize>| -> Vec<String> {
            picks.into_iter().map(|i| c.tasks[i].id.clone()).collect()
        };
        let artefact: Vec<String> = c
            .tasks
            .iter()
            .filter(|t| t.has_tag(ARTEFACT_TAG))
            .map(|t| t.id.clone())
            .collect();
        assert_eq!(artefact.len(), 7);
        // Every task after the first SPREAD_TASKS is an artefact task, so none is left out.
        assert!(
            c.tasks[SPREAD_TASKS..]
                .iter()
                .all(|t| t.has_tag(ARTEFACT_TAG))
        );
        // The 16 tasks of `--local-models --scale standard` before EE2 (run 41007073954), then
        // the artefact tasks; likewise 8 (`quick`) and 3 (`smoke`).
        let before_16 = [
            "maya-t01",
            "maya-t05",
            "maya-t09",
            "maya-t13",
            "maya-t17",
            "maya-t21",
            "henrik-t03",
            "henrik-t07",
            "henrik-t12",
            "henrik-t16",
            "henrik-t20",
            "jules-t02",
            "jules-t06",
            "jules-t10",
            "jules-t14",
            "jules-t18",
        ];
        let before_8 = [
            "maya-t01",
            "maya-t09",
            "maya-t17",
            "henrik-t03",
            "henrik-t12",
            "henrik-t20",
            "jules-t06",
            "jules-t14",
        ];
        let before_3 = ["maya-t01", "henrik-t01", "jules-t01"];
        for (limit, before) in [
            (16, &before_16[..]),
            (8, &before_8[..]),
            (3, &before_3[..]),
            (20, &[][..]),
        ] {
            let picked = ids(select_tasks(&c, limit, true));
            let (old, new): (Vec<String>, Vec<String>) = picked
                .iter()
                .cloned()
                .partition(|id| !artefact.contains(id));
            if !before.is_empty() {
                assert_eq!(old, before, "limit {limit}");
            }
            assert_eq!(old, ids(spread(SPREAD_TASKS, limit)), "limit {limit}");
            assert_eq!(new, artefact, "limit {limit}");
        }
        // The full corpus when the limit covers it.
        assert_eq!(select_tasks(&c, c.tasks.len(), true).len(), c.tasks.len());
        // `smoke` stays the spread alone.
        assert_eq!(select_tasks(&c, 6, false), spread(SPREAD_TASKS, 6));
    }

    // ---- real-model paths, with in-process providers (no network) ----

    use async_trait::async_trait;
    use nugget_core::ids::ProviderId;
    use nugget_infer::{ChatStream, ProviderKind, ProviderMeta, StopReason};

    /// What a scripted provider does with a request.
    type Script = dyn Fn(&ChatRequest) -> Result<String, nugget_core::Error> + Send + Sync;

    struct Scripted {
        id: ProviderId,
        script: Box<Script>,
        /// Never answer (for timeouts).
        hang: bool,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Scripted {
        fn new(
            script: impl Fn(&ChatRequest) -> Result<String, nugget_core::Error> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Self {
                id: ProviderId::new("scripted"),
                script: Box::new(script),
                hang: false,
                calls: Default::default(),
            })
        }
    }

    #[async_trait]
    impl Provider for Scripted {
        fn id(&self) -> &ProviderId {
            &self.id
        }
        fn meta(&self) -> ProviderMeta {
            ProviderMeta {
                kind: ProviderKind::OpenaiCompatible,
                model: "scripted".into(),
                locality: Locality::OnDevice,
                context_tokens: Some(8192),
            }
        }
        async fn chat(&self, req: ChatRequest) -> nugget_core::Result<ChatStream> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.hang {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
            let text = (self.script)(&req)?;
            Ok(Box::pin(futures::stream::iter([
                Ok(ChatEvent::TextDelta(text)),
                Ok(ChatEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    input_tokens: None,
                    output_tokens: None,
                }),
            ])))
        }
        async fn embed(&self, _: &[String]) -> nugget_core::Result<Vec<Vec<f32>>> {
            Err(nugget_core::Error::Unsupported("embed".into()))
        }
    }

    fn real(provider: Arc<dyn Provider>, timeout: Duration) -> Model {
        Model::Real(RealModel {
            provider,
            timeout,
            max_output: DRAFT_MAX_TOKENS,
            temperature: Some(DEFAULT_TEMPERATURE),
            _local: None,
        })
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn classifies_provider_failures() {
        assert_eq!(
            failure_kind(
                "provider error: HTTP 400: the request exceeds the available context size, try \
                 increasing it"
            ),
            "context_overflow"
        );
        assert_eq!(
            failure_kind("This model's maximum context length is 8192 tokens"),
            "context_overflow"
        );
        assert_eq!(
            failure_kind("prompt is too long: 210000 tokens"),
            "context_overflow"
        );
        assert_eq!(failure_kind("network error: connection refused"), "error");
    }

    #[test]
    fn budget_leaves_room_for_output_and_margin() {
        assert_eq!(prompt_budget(8192, 400), 6405);
        assert_eq!(prompt_budget(100, 400), 0);
    }

    #[test]
    fn failed_requests_are_counted_scored_empty_and_forfeit_the_judge() {
        let world = World::build(Corpus::load().unwrap()).unwrap();
        // Only Nugget's prompt has a voice profile: the baseline's requests all fail.
        let p = Scripted::new(|req| {
            let user = match &req.messages[0].content[0] {
                nugget_infer::Part::Text { text } => text.clone(),
                _ => String::new(),
            };
            if user.contains("### VOICE PROFILE") {
                Ok("Hi,\n\nSounds good.\n\nBest".into())
            } else {
                Err(nugget_core::Error::Provider(
                    "HTTP 400: the request exceeds the available context size".into(),
                ))
            }
        });
        let model = real(p.clone(), Duration::from_secs(30));
        let tasks = [0, 1, 2, 3];
        let run = block_on(run_arms(&world, &tasks, &model, false, 1, None, None)).unwrap();
        assert_eq!(p.calls.load(std::sync::atomic::Ordering::SeqCst), 8);
        assert_eq!(run.failures("nugget"), 0);
        assert_eq!(run.failures("baseline"), 4);
        assert_eq!(run.failure_kinds()["baseline"]["context_overflow"], 4);
        assert!(run.baseline.iter().all(|s| s.failed && s.gold_hit == 0));
        assert!(run.baseline_drafts.iter().all(String::is_empty));
        assert!(run.nugget.iter().all(|s| !s.failed));
        let mut out = metrics(&run, true, Locality::OnDevice);
        failure_metrics(&run, &mut out);
        let get = |n: &str| out.iter().find(|m| m.name == n).unwrap().value;
        assert_eq!(get("draft.baseline.failures"), 4.0);
        assert_eq!(get("draft.nugget.failures"), 0.0);
        // The judge gives every task to the arm that produced a draft, without a call.
        let judgements: Vec<_> = run
            .nugget
            .iter()
            .zip(&run.baseline)
            .map(|(n, b)| judge::heuristic(n, b))
            .collect();
        let j = judge::summarise(&judgements);
        assert_eq!((j.nugget_win_rate, j.forfeits), (1.0, 4));
        let mj = ModelJudge {
            provider: p.clone(),
            timeout: Duration::from_secs(5),
            max_tokens: JUDGE_MAX_TOKENS,
        };
        let js = block_on(judge_all(&world, &tasks, &run, &mj, 1)).unwrap();
        assert_eq!(p.calls.load(std::sync::atomic::Ordering::SeqCst), 8);
        assert!(js.iter().all(|j| j.winner.as_str() == "nugget_by_forfeit"));
    }

    #[test]
    fn a_hanging_request_times_out_and_counts() {
        let world = World::build(Corpus::load().unwrap()).unwrap();
        let p = Arc::new(Scripted {
            id: ProviderId::new("hang"),
            script: Box::new(|_| Ok(String::new())),
            hang: true,
            calls: Default::default(),
        });
        let model = real(p, Duration::from_millis(50));
        let run = block_on(run_arms(&world, &[0], &model, false, 1, None, None)).unwrap();
        assert_eq!(run.failures("nugget") + run.failures("baseline"), 2);
        assert_eq!(run.failure_kinds()["nugget"]["timeout"], 1);
        // Failed requests stay out of the latency percentiles.
        let mut out = metrics(&run, true, Locality::OnDevice);
        failure_metrics(&run, &mut out);
        assert_eq!(
            out.iter()
                .find(|m| m.name == "draft.nugget.ttft.p50_s")
                .unwrap()
                .value,
            0.0
        );
    }

    #[test]
    fn real_drafts_lose_inline_thinking_and_baseline_fits_the_budget() {
        let world = World::build(Corpus::load().unwrap()).unwrap();
        let p = Scripted::new(|_| Ok("<think>plan the reply</think>\nHi Sam, yes.".into()));
        let model = real(p, Duration::from_secs(30));
        let budget = prompt_budget(4096, DRAFT_MAX_TOKENS);
        let run = block_on(run_arms(
            &world,
            &[0, 5],
            &model,
            false,
            1,
            Some(budget),
            None,
        ))
        .unwrap();
        assert_eq!(run.nugget_drafts[0], "Hi Sam, yes.");
        assert_eq!(run.baseline_truncated, 2);
        assert!(run.baseline.iter().all(|s| s.prompt_tokens <= budget));
        // Nugget's prompt is never cut.
        assert!(run.nugget.iter().all(|s| s.prompt_tokens < budget));
    }

    #[test]
    fn model_judge_cancels_position_bias_and_retries_without_schema() {
        let c = Corpus::load().unwrap();
        let t = &c.tasks[0];
        let input = JudgeInput::new(t, c.persona(&t.persona).unwrap());
        let text_of = |req: &ChatRequest| match &req.messages[0].content[0] {
            nugget_infer::Part::Text { text } => text.clone(),
            _ => String::new(),
        };
        let judge = |p: Arc<Scripted>| ModelJudge {
            provider: p,
            timeout: Duration::from_secs(5),
            max_tokens: JUDGE_MAX_TOKENS,
        };
        // Always "A": every task is inconsistent.
        let biased = Scripted::new(|_| Ok(r#"{"voice":"A","factuality":"A","winner":"A"}"#.into()));
        let j = block_on(judge(biased.clone()).judge(&input, "ZXGOOD", "ZXBAD", "n"));
        assert_eq!(j.winner, judge::Outcome::Inconsistent);
        assert_eq!(biased.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        // Picks the draft that says "good", wrapped in thinking; rejects the schema once per call.
        let fair = Scripted::new(move |req| {
            if req.json_schema.is_some() {
                return Err(nugget_core::Error::Provider(
                    "response_format unsupported".into(),
                ));
            }
            let u = text_of(req);
            let a_good = u.find("ZXGOOD").unwrap() < u.find("ZXBAD").unwrap();
            let w = if a_good { "A" } else { "B" };
            Ok(format!(
                "<think>hmm</think>{{\"voice\":\"tie\",\"winner\":\"{w}\"}}"
            ))
        });
        let j = block_on(judge(fair.clone()).judge(&input, "ZXGOOD", "ZXBAD", "n"));
        assert_eq!(
            j.winner,
            judge::Outcome::Agreed(judge::Side::Nugget),
            "{j:?}"
        );
        assert_eq!(j.voice, judge::Outcome::Agreed(judge::Side::Tie));
        assert_eq!(j.factuality, judge::Outcome::Failed);
        assert_eq!(fair.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
        // Garbage is a judge failure, not a crash.
        let garbage = Scripted::new(|_| Ok("I cannot decide.".into()));
        let j = block_on(judge(garbage).judge(&input, "ZXGOOD", "ZXBAD", "n"));
        assert_eq!(j.winner, judge::Outcome::Failed);
        assert_eq!(j.failure, Some("unparseable"));
    }

    #[test]
    fn heuristic_judge_over_the_fake_model() {
        let r = smoke(DraftOptions {
            draft_judge: Some(JudgeMode::Heuristic),
            ..DraftOptions::default()
        });
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert!(
            r.config.params["judge"]
                .as_str()
                .unwrap()
                .starts_with("heuristic")
        );
        assert_eq!(value(&r, "draft.judge.inconsistent_rate"), 0.0);
        assert_eq!(value(&r, "draft.judge.failures"), 0.0);
        let (n, b, t) = (
            value(&r, "draft.judge.nugget_win_rate"),
            value(&r, "draft.judge.baseline_win_rate"),
            value(&r, "draft.judge.tie_rate"),
        );
        assert!((n + b + t - 1.0).abs() < 1e-9);
        assert!(n > b);
        // Same composite as the suite's own pairwise win rate (ties half).
        assert!((n + t / 2.0 - value(&r, "draft.win_rate_vs_baseline")).abs() < 1e-9);
        // The default fake run has no judge, no failure counts and no truncation.
        let plain = smoke(DraftOptions::default());
        assert_eq!(plain.config.params["judge"], "none");
        assert!(plain.metrics.iter().all(|m| !m.name.contains("judge")
            && !m.name.ends_with("failures")
            && !m.name.contains("truncated")));
        // The fake model cannot judge.
        let bad = smoke(DraftOptions {
            draft_judge: Some(JudgeMode::Model),
            ..DraftOptions::default()
        });
        assert_eq!(bad.status, Status::Failed);
    }

    #[test]
    fn parses_the_new_options() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            d: DraftOptions,
        }
        let c = Cli::try_parse_from(["x"]).unwrap();
        assert_eq!(c.d, DraftOptions::default());
        let c = Cli::try_parse_from([
            "x",
            "--draft-judge",
            "heuristic",
            "--draft-context-tokens",
            "4096",
            "--draft-timeout-s",
            "60",
        ])
        .unwrap();
        assert_eq!(c.d.draft_judge, Some(JudgeMode::Heuristic));
        assert_eq!(c.d.draft_context_tokens, Some(4096));
        assert_eq!(c.d.draft_timeout_s, Some(60));
        assert!(Cli::try_parse_from(["x", "--draft-judge", "gpt"]).is_err());
        let c = Cli::try_parse_from([
            "x",
            "--draft-grounding",
            "both",
            "--draft-temperature",
            "0.2",
            "--draft-judge-model",
            "qwen3-8b-q4km",
            "--draft-judge-timeout-s",
            "300",
            "--draft-judge-grounding",
        ])
        .unwrap();
        assert_eq!(c.d.draft_grounding, Some(GroundingMode::Both));
        assert_eq!(c.d.draft_temperature, Some(Temperature(0.2)));
        assert_eq!(c.d.draft_judge_model.as_deref(), Some("qwen3-8b-q4km"));
        assert_eq!(c.d.draft_judge_timeout_s, Some(300));
        assert!(c.d.draft_judge_grounding);
        for bad in [
            ["x", "--draft-grounding", "maybe"],
            ["x", "--draft-temperature", "3"],
            ["x", "--draft-temperature", "warm"],
        ] {
            assert!(Cli::try_parse_from(bad).is_err(), "{bad:?}");
        }
        // Defaults: both arms with a real model, grounded only with the fake one.
        assert_eq!(GroundingMode::resolve(None, true), GroundingMode::Both);
        assert_eq!(GroundingMode::resolve(None, false), GroundingMode::On);
        assert_eq!(
            GroundingMode::resolve(Some(GroundingMode::Off), true),
            GroundingMode::Off
        );
    }

    #[test]
    fn both_mode_over_the_fake_reports_two_nugget_arms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drafts.jsonl");
        let r = smoke(DraftOptions {
            draft_grounding: Some(GroundingMode::Both),
            draft_dump: Some(path.clone()),
            ..DraftOptions::default()
        });
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert_eq!(r.config.params["nugget.grounding"], "both");
        for name in [
            "voice_match",
            "fact_recall",
            "hallucination_rate",
            "sensitive_leak_rate",
            "injection_resistance",
            "prompt_tokens.p50",
            "ttft.p50_s",
            "total.p50_s",
        ] {
            assert!(
                r.metric(&format!("draft.nugget.{name}")).is_some(),
                "{name}"
            );
            let un = format!("draft.nugget_ungrounded.{name}");
            assert!(r.metric(&un).is_some(), "{un}");
            // No targets on the second arm: it is there for comparison.
            assert!(r.metric(&un).unwrap().target.is_none(), "{un}");
        }
        // The in-suite pipeline has no grounding switch: both arms are the same here.
        assert_eq!(
            value(&r, "draft.nugget.voice_match"),
            value(&r, "draft.nugget_ungrounded.voice_match")
        );
        for t in &r.targets {
            assert!(t.pass, "{t:?}");
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 3 * 73);
        let arms: std::collections::BTreeSet<String> = text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["arm"].to_string())
            .collect();
        assert_eq!(arms.len(), 3, "{arms:?}");
        assert!(arms.contains("\"nugget_ungrounded\""));
        // The default fake run has one Nugget arm, grounding on.
        let plain = smoke(DraftOptions::default());
        assert_eq!(plain.config.params["nugget.grounding"], "on");
        assert!(plain.metrics.iter().all(|m| !m.name.contains("ungrounded")));
    }

    #[test]
    fn grounding_judge_is_opt_in_and_needs_both_arms() {
        let r = smoke(DraftOptions {
            draft_grounding: Some(GroundingMode::Both),
            draft_judge: Some(JudgeMode::Heuristic),
            draft_judge_grounding: true,
            ..DraftOptions::default()
        });
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        // Identical arms: every task is a tie.
        assert_eq!(value(&r, "draft.judge_grounding.tie_rate"), 1.0);
        assert!(
            r.metric("draft.judge_grounding.grounded_win_rate")
                .is_some()
        );
        assert!(
            r.metric("draft.judge_grounding.ungrounded_win_rate")
                .is_some()
        );
        assert!(
            r.metric("draft.judge_grounding.factuality.grounded_win_rate")
                .is_some()
        );
        let bad = smoke(DraftOptions {
            draft_judge: Some(JudgeMode::Heuristic),
            draft_judge_grounding: true,
            ..DraftOptions::default()
        });
        assert_eq!(bad.status, Status::Failed);
    }

    #[test]
    fn judge_plan_follows_the_options() {
        use JudgePlan as P;
        let m = Some(JudgeMode::Model);
        // Defaults: model with --local-models, none otherwise.
        assert_eq!(
            plan_judge(None, None, true, true, Some("a")),
            Ok(P::SameModel)
        );
        assert_eq!(plan_judge(None, None, true, false, None), Ok(P::None));
        assert_eq!(plan_judge(None, None, false, false, None), Ok(P::None));
        assert_eq!(
            plan_judge(Some(JudgeMode::Heuristic), None, false, false, None),
            Ok(P::Heuristic)
        );
        // A different judge model: a second local server.
        assert_eq!(
            plan_judge(None, Some("big"), true, true, Some("small")),
            Ok(P::LocalModel("big".into()))
        );
        assert_eq!(
            plan_judge(m, Some("big"), true, true, Some("small")),
            Ok(P::LocalModel("big".into()))
        );
        // The drafting model named as judge is the same model.
        assert_eq!(
            plan_judge(m, Some("small"), true, true, Some("small")),
            Ok(P::SameModel)
        );
        // Contradictions and impossible setups.
        assert!(plan_judge(Some(JudgeMode::Heuristic), Some("big"), true, true, None).is_err());
        assert!(plan_judge(Some(JudgeMode::None), Some("big"), true, true, None).is_err());
        assert!(plan_judge(m, Some("big"), true, false, None).is_err());
        assert!(plan_judge(m, None, false, false, None).is_err());
        assert!(plan_judge(None, Some("big"), false, false, None).is_err());
        assert_eq!(P::LocalModel("x".into()).mode(), JudgeMode::Model);
        // The fake model cannot use a judge model.
        let bad = smoke(DraftOptions {
            draft_judge_model: Some("qwen3-8b-q4km".into()),
            ..DraftOptions::default()
        });
        assert_eq!(bad.status, Status::Failed);
    }

    #[test]
    fn heuristic_judge_runs_next_to_any_judge() {
        let r = smoke(DraftOptions {
            draft_judge: Some(JudgeMode::Heuristic),
            ..DraftOptions::default()
        });
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        for name in [
            "nugget_win_rate",
            "baseline_win_rate",
            "tie_rate",
            "inconsistent_rate",
            "voice.nugget_win_rate",
            "factuality.nugget_win_rate",
            "failures",
        ] {
            assert_eq!(
                value(&r, &format!("draft.judge_heuristic.{name}")),
                value(&r, &format!("draft.judge.{name}")),
                "{name}"
            );
        }
    }

    #[test]
    fn engine_arms_draft_through_the_drafter_at_a_fixed_temperature() {
        let world = World::build(Corpus::load().unwrap()).unwrap();
        let temps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = temps.clone();
        let p = Scripted::new(move |req| {
            seen.lock().unwrap().push(req.temperature);
            Ok("Hi Sam, yes, that works.".into())
        });
        let model = real(p.clone(), Duration::from_secs(30));
        let engine = |grounding| {
            let config = nugget_draft::DraftConfig {
                grounding,
                temperature: Some(0.0),
                ..nugget_draft::DraftConfig::default()
            };
            Engine::new(&world, p.clone(), config, None, Duration::from_secs(30))
        };
        let (on, off) = (engine(true), engine(false));
        let setup = ArmSetup {
            nugget: NuggetImpl::Engine(&on),
            ungrounded: Some(NuggetImpl::Engine(&off)),
            local_budget: None,
        };
        let tasks = [0, 1, 2];
        let run = block_on(run_arms_with(&world, &tasks, &model, false, 1, None, setup)).unwrap();
        assert_eq!(p.calls.load(std::sync::atomic::Ordering::SeqCst), 9);
        assert!(temps.lock().unwrap().iter().all(|t| *t == Some(0.0)));
        assert_eq!(run.arms(), ["nugget", UNGROUNDED_ARM, "baseline"]);
        for drafts in [&run.nugget_drafts, &run.ungrounded_drafts] {
            assert!(
                drafts.iter().all(|d| d.contains("that works")),
                "{drafts:?}"
            );
        }
        assert!(
            run.ungrounded
                .iter()
                .all(|s| !s.failed && s.prompt_tokens > 100)
        );
        assert_eq!(run.field_kind_correct, 3);
        let mut out = metrics(&run, true, Locality::OnDevice);
        failure_metrics(&run, &mut out);
        assert!(
            out.iter()
                .any(|m| m.name == "draft.nugget_ungrounded.failures")
        );
        assert_eq!(run.failure_kinds().as_object().unwrap().len(), 3);
        // The grounded arm judged against the ungrounded one, in both orders.
        let mj = ModelJudge {
            provider: Scripted::new(|_| {
                Ok(r#"{"voice":"tie","factuality":"tie","winner":"tie"}"#.into())
            }),
            timeout: Duration::from_secs(5),
            max_tokens: JUDGE_MAX_TOKENS,
        };
        let js = block_on(judge_pairs(
            &world,
            &tasks,
            (&run.nugget_drafts, &run.nugget),
            (&run.ungrounded_drafts, &run.ungrounded),
            &mj,
            1,
        ))
        .unwrap();
        assert_eq!(judge::summarise(&js).tie_rate, 1.0);
    }

    // ---- an OpenAI-compatible server on loopback (wiremock) ----

    fn mock_run(
        status: u16,
        body: &str,
        sse: bool,
        extra: DraftOptions,
    ) -> (Report, Vec<wiremock::Request>) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(MockServer::start());
        let mut resp = ResponseTemplate::new(status).set_body_string(body.to_owned());
        if sse {
            resp = resp.insert_header("content-type", "text/event-stream");
        }
        rt.block_on(
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .respond_with(resp)
                .mount(&server),
        );
        let report = smoke(DraftOptions {
            provider_url: Some(format!("{}/v1", server.uri())),
            provider_model: Some("mock".into()),
            ..extra
        });
        let reqs = rt.block_on(server.received_requests()).unwrap();
        (report, reqs)
    }

    #[test]
    fn suite_fails_only_when_every_request_failed() {
        let (r, reqs) = mock_run(
            400,
            r#"{"error":{"code":400,"message":"the request exceeds the available context size","type":"exceed_context_size_error"}}"#,
            false,
            DraftOptions::default(),
        );
        assert_eq!(r.status, Status::Failed);
        assert!(
            r.message
                .as_deref()
                .unwrap()
                .contains("every draft request failed"),
            "{:?}",
            r.message
        );
        // 6 tasks x 3 arms (grounded, ungrounded, baseline).
        assert_eq!(reqs.len(), 18);
    }

    #[test]
    fn context_option_cuts_the_baseline_and_reports_it() {
        let ev = json!({
            "object": "chat.completion.chunk",
            "choices": [{ "index": 0, "delta": { "content": "Hi, yes." }, "finish_reason": null }],
        });
        let body = format!(
            "data: {ev}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
        );
        let (r, reqs) = mock_run(
            200,
            &body,
            true,
            DraftOptions {
                draft_context_tokens: Some(4096),
                ..DraftOptions::default()
            },
        );
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        // No judge by default with --provider-url: 6 tasks x 3 arms (both Nugget arms through
        // the engine, then the baseline), every request at temperature 0.
        assert_eq!(reqs.len(), 18);
        for req in &reqs {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["temperature"], 0.0, "{body}");
        }
        let p = &r.config.params;
        assert_eq!(p["baseline.context_tokens"], 4096);
        assert_eq!(p["baseline.context_tokens_source"], "draft_context_tokens");
        let budget = prompt_budget(4096, DRAFT_MAX_TOKENS) as f64;
        assert_eq!(p["baseline.prompt_budget_tokens"], budget as u64);
        assert_eq!(value(&r, "draft.baseline.truncated_tasks"), 6.0);
        assert!(value(&r, "draft.baseline.prompt_tokens.p95") <= budget);
        assert_eq!(value(&r, "draft.nugget.failures"), 0.0);
        assert_eq!(value(&r, "draft.nugget_ungrounded.failures"), 0.0);
        assert_eq!(value(&r, "draft.baseline.failures"), 0.0);
        assert_eq!(p["judge"], "none");
        assert_eq!(p["nugget.grounding"], "both");
        assert_eq!(p["sampling.temperature"], 0.0);
        assert_eq!(p["sampling.seed"], SEED_NOTE);
        assert!(p["nugget_path"].as_str().unwrap().starts_with("engine"));
        // The engine's prompts are scored like the others.
        assert!(value(&r, "draft.nugget.prompt_tokens.p50") > 100.0);
        assert!(value(&r, "draft.nugget_ungrounded.prompt_tokens.p50") > 100.0);
    }

    #[test]
    fn grounding_off_runs_one_engine_arm_as_nugget() {
        let ev = json!({
            "object": "chat.completion.chunk",
            "choices": [{ "index": 0, "delta": { "content": "Hi, yes." }, "finish_reason": null }],
        });
        let body = format!(
            "data: {ev}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
        );
        let (r, reqs) = mock_run(
            200,
            &body,
            true,
            DraftOptions {
                draft_grounding: Some(GroundingMode::Off),
                draft_temperature: Some(Temperature(0.3)),
                ..DraftOptions::default()
            },
        );
        assert_eq!(r.status, Status::Ok, "{:?}", r.message);
        assert_eq!(reqs.len(), 12);
        for req in &reqs {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert!((body["temperature"].as_f64().unwrap() - 0.3).abs() < 1e-6);
        }
        assert_eq!(r.config.params["nugget.grounding"], "off");
        assert!(r.metric("draft.nugget.voice_match").is_some());
        assert!(r.metrics.iter().all(|m| !m.name.contains("ungrounded")));
    }

    /// Gold facts of one task that reached the prompt the engine sends for it.
    struct PromptGold {
        task: String,
        /// Per gold fact: its label, whether its any-of appears in the whole prompt and in the
        /// facts block (`<context>` … `</context>`).
        facts: Vec<(String, bool, bool)>,
        prompt: String,
    }

    /// Draft `tasks` through [`Engine`] with a scripted on-device model (so the local budget
    /// applies when `local_budget` is on) and record which gold facts reached each prompt.
    fn engine_prompt_gold(world: &World, tasks: &[usize], local_budget: bool) -> Vec<PromptGold> {
        let p = Scripted::new(|_| Ok("ok".into()));
        let mut config = nugget_draft::DraftConfig {
            temperature: Some(0.0),
            ..nugget_draft::DraftConfig::default()
        };
        config.local.enabled = local_budget;
        let engine = Engine::new(world, p, config, None, Duration::from_secs(30));
        tasks
            .iter()
            .map(|&i| {
                let task = &world.corpus.tasks[i];
                let (_, prompt) = block_on(engine.draft(task, Instant::now()));
                let lower = prompt.to_lowercase();
                let start = lower.find("<context>").unwrap_or(0);
                let end = lower[start..]
                    .find("</context>")
                    .map_or(start, |e| start + e);
                let block = &lower[start..end];
                let facts = task
                    .gold_facts
                    .iter()
                    .map(|f| {
                        (
                            f.label.clone(),
                            fixtures::contains_any(&lower, &f.any_of),
                            fixtures::contains_any(block, &f.any_of),
                        )
                    })
                    .collect();
                PromptGold {
                    task: task.id.clone(),
                    facts,
                    prompt,
                }
            })
            .collect()
    }

    /// Gold facts in the facts block (or, with `whole_prompt`, anywhere in the prompt), and
    /// all gold facts.
    fn gold_count(rows: &[PromptGold], whole_prompt: bool) -> (usize, usize) {
        let facts = rows.iter().flat_map(|r| &r.facts);
        let hit = |f: &&(String, bool, bool)| if whole_prompt { f.1 } else { f.2 };
        (facts.clone().filter(hit).count(), facts.count())
    }

    /// Floor on the gold facts that reach the local grounded prompt's facts block over the whole
    /// corpus (of 100; `jules-t24`'s "poster" is only ever in the thread). Set from the tree
    /// while EE2's retrieval work was in progress: raise it as retrieval improves.
    const LOCAL_PROMPT_GOLD_FLOOR: usize = 92;

    /// Offline recall regression: how many gold facts the local grounded prompt carries over
    /// every task, through the real engine and the local budget.
    #[test]
    fn gold_facts_reach_the_local_grounded_prompt() {
        let world = World::build(Corpus::load().unwrap()).unwrap();
        let all: Vec<usize> = (0..world.corpus.tasks.len()).collect();
        let rows = engine_prompt_gold(&world, &all, true);
        // The facts block only: a gold string already in the thread, the system prompt or an
        // exemplar is not retrieval's doing.
        let (hit, total) = gold_count(&rows, false);
        let (old_hit, old_total) = gold_count(&rows[..SPREAD_TASKS], false);
        let (anywhere, _) = gold_count(&rows, true);
        let missed: Vec<String> = rows
            .iter()
            .flat_map(|r| {
                r.facts
                    .iter()
                    .filter(|f| !f.2)
                    .map(move |f| format!("{}:{}", r.task, f.0))
            })
            .collect();
        println!(
            "gold facts in the local grounded facts block: {hit} of {total} (first \
             {SPREAD_TASKS} tasks: {old_hit} of {old_total}; anywhere in the prompt: \
             {anywhere}); missed: {missed:?}"
        );
        assert!(
            hit >= LOCAL_PROMPT_GOLD_FLOOR,
            "{hit} of {total} gold facts reach the local facts block, floor \
             {LOCAL_PROMPT_GOLD_FLOOR}; missed: {missed:?}"
        );
    }

    /// Diagnostic: print every prompt's facts block and which gold facts reached it, with the
    /// local and the cloud budget. `DIAG_ALL=1` for every task (default: the 16-task selection).
    #[test]
    #[ignore]
    fn diag_dump_prompts() {
        let world = World::build(Corpus::load().unwrap()).unwrap();
        let tasks: Vec<usize> = if std::env::var("DIAG_ALL").is_ok() {
            (0..world.corpus.tasks.len()).collect()
        } else {
            select_tasks(&world.corpus, 16, true)
        };
        for (label, budget) in [("local", true), ("cloud", false)] {
            let rows = engine_prompt_gold(&world, &tasks, budget);
            for (r, &i) in rows.iter().zip(&tasks) {
                let task = &world.corpus.tasks[i];
                let start = r.prompt.find("<context>").unwrap_or(0);
                let end = r.prompt[start..]
                    .find("</context>")
                    .map_or(start, |e| start + e);
                let gold: Vec<String> = r
                    .facts
                    .iter()
                    .zip(&task.gold_facts)
                    .map(|((l, p, f), g)| format!("{l}[{:?}] prompt={p} facts={f}", g.items))
                    .collect();
                println!(
                    "=== {label} {} tags={:?} tokens~{}\n gold: {}\n{}\n",
                    r.task,
                    task.tags,
                    r.prompt.len() / 4,
                    gold.join(" | "),
                    &r.prompt[start..end]
                );
            }
            let (block, total) = gold_count(&rows, false);
            let (anywhere, _) = gold_count(&rows, true);
            println!(
                "=== {label}: {block} of {total} gold facts in the facts block, {anywhere} in the \
                 prompt"
            );
        }
    }
}
