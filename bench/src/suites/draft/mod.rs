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
//! Sentient's own cap does. [`judge`] adds a pairwise judge (`--draft-judge`).

pub mod arms;
pub mod fake;
pub mod fixtures;
pub mod judge;
pub mod prompt;
pub mod score;
pub mod style;

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
    /// `draft`, `doubletap`: the smaller prompt budget `nugget-draft` gives non-cloud providers
    /// (`nugget_draft::LocalBudget`): `on` (default, as the app) or `off` (the cloud budget for
    /// every provider), for before/after comparisons on one job.
    #[arg(long, value_enum, default_value_t = Switch::On)]
    pub draft_local_budget: Switch,
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
    /// The servers behind `provider` (`--local-models`); dropping it stops them.
    _local: Option<Box<LocalModels>>,
}

enum Model {
    Fake(fake::Latency),
    Real(RealModel),
}

struct Generation {
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
}

impl Model {
    fn build(
        o: &DraftOptions,
        local: &LocalOptions,
        ctx: &mut RunContext,
    ) -> Result<Built, BenchError> {
        let timeout = Duration::from_secs(o.draft_timeout_s.unwrap_or(DEFAULT_TIMEOUT_S).max(1));
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
                    _local: Some(Box::new(models)),
                }),
                locality: meta.locality,
                desc,
                context_tokens: meta.context_tokens,
                local: true,
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
                _local: None,
            }),
            locality: meta.locality,
            desc: format!("{kind} {url} model {model}"),
            context_tokens: meta.context_tokens,
            local: false,
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
                let req = ChatRequest::new(vec![Message::user_text(p.user.clone())])
                    .with_system(p.system.clone())
                    .with_max_tokens(real.max_output)
                    .with_feature("draft");
                let fail = |kind: &'static str, message: String| RequestFailure {
                    kind,
                    message,
                    elapsed: started.elapsed(),
                };
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
                let (text, ttft) = match tokio::time::timeout(real.timeout, stream).await {
                    Err(_) => {
                        return Err(fail(
                            "timeout",
                            format!("no complete draft within {} s", real.timeout.as_secs()),
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
    /// Tasks whose baseline prompt was cut to the budget.
    pub baseline_truncated: usize,
    pub field_kind_correct: usize,
    pub recipient_profiles: usize,
    pub excluded_sensitive: u32,
    pub neutralised: usize,
    /// Per task, when a judge ran.
    pub judgements: Vec<TaskJudgement>,
}

impl DraftRun {
    pub fn failures(&self, arm: &str) -> usize {
        let errs = if arm == "nugget" {
            &self.nugget_errors
        } else {
            &self.baseline_errors
        };
        errs.iter().flatten().count()
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
        json!({
            "nugget": count(&self.nugget_errors),
            "baseline": count(&self.baseline_errors),
        })
    }
}

/// Run both arms over `tasks` (indices into the corpus) and score them. `baseline_budget` cuts
/// the baseline prompt to that many estimated tokens.
async fn run_arms(
    world: &World,
    tasks: &[usize],
    model: &Model,
    exclude_sensitive: bool,
    seed: u64,
    baseline_budget: Option<usize>,
    local_budget: Option<nugget_draft::LocalBudget>,
) -> Result<DraftRun, BenchError> {
    let nugget_arm = NuggetArm::with_local_budget(world, local_budget);
    let baseline_arm = BaselineArm::new(world, seed);
    let mut run = DraftRun::default();
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
        // Alternate which arm goes first, so neither always gets a warm connection.
        for arm in if n % 2 == 0 { [0, 1] } else { [1, 0] } {
            let started = Instant::now();
            let prompt = if arm == 0 {
                let (p, notes) = nugget_arm.prompt(task, exclude_sensitive).await?;
                run.field_kind_correct +=
                    usize::from(notes.field_kind.as_str() == task.expected_kind);
                run.recipient_profiles += usize::from(notes.voice_scope == "recipient");
                run.excluded_sensitive += notes.excluded_sensitive;
                run.neutralised += notes.neutralised;
                p
            } else {
                let (p, cut) = baseline_arm.prompt_within(task, baseline_budget)?;
                run.baseline_truncated += usize::from(cut);
                p
            };
            let prompt_text = prompt.text();
            let input = |draft, ttft, total| ScoreInput {
                task,
                draft,
                prompt_text: &prompt_text,
                prompt_tokens: prompt.tokens(),
                channel_profile: &channel_profile,
                secrets: &world.corpus.secrets,
                ttft,
                total,
            };
            let (s, text, err) = match model.generate(&prompt, started).await {
                Ok(g) => (score::score(&input(&g.text, g.ttft, g.total)), g.text, None),
                Err(f) => {
                    eprintln!(
                        "draft: {} {} request failed ({}): {}",
                        task.id,
                        if arm == 0 { "nugget" } else { "baseline" },
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
                    if arm == 0 { "nugget" } else { "baseline" },
                    s.prompt_tokens,
                    s.ttft.as_secs_f64(),
                    s.total.as_secs_f64()
                );
            }
            if arm == 0 {
                run.nugget.push(s);
                run.nugget_drafts.push(text);
                run.nugget_errors.push(err);
            } else {
                run.baseline.push(s);
                run.baseline_drafts.push(text);
                run.baseline_errors.push(err);
            }
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
        let Built {
            model,
            locality,
            desc: model_desc,
            context_tokens,
            local,
        } = Model::build(&opts, &local_opts, ctx)?;
        let real = matches!(model, Model::Real(_));
        let cloud = locality != Locality::OnDevice;
        let judge_mode = opts.draft_judge.unwrap_or(if local {
            JudgeMode::Model
        } else {
            JudgeMode::None
        });
        if judge_mode == JudgeMode::Model && !real {
            return Err(BenchError::Failed(
                "--draft-judge model needs a real model (--local-models or --provider-url); the \
                 fake model can use --draft-judge heuristic"
                    .into(),
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
        let tasks: Vec<usize> = spread(all, limit);

        ctx.param("model", model_desc.clone());
        ctx.param("model.locality", format!("{locality:?}"));
        ctx.param("nugget_path", NUGGET_PATH);
        ctx.param("baseline_path", BASELINE_PATH);
        ctx.param("voice_metric", VOICE_METRIC);
        ctx.param("tasks", tasks.len());
        ctx.param("tasks.available", all);
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
        }
        ctx.param("judge", judge_description(judge_mode, &model_desc));
        ctx.param("judge.mode", judge_mode.as_str());
        if judge_mode == JudgeMode::Model {
            ctx.param("judge.same_model_as_drafts", true);
        }

        let world = World::build(corpus)?;
        let vault_bytes = BaselineArm::new(&world, ctx.seed()).vault_bytes();
        ctx.param("baseline.vault_bytes", vault_bytes);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut run = rt.block_on(run_arms(
            &world,
            &tasks,
            &model,
            cloud,
            ctx.seed(),
            baseline_budget,
            local_budget,
        ))?;
        ctx.param("nugget.excluded_sensitive", run.excluded_sensitive);
        ctx.param("nugget.neutralised_sentences", run.neutralised);
        ctx.param("nugget.recipient_profiles", run.recipient_profiles);
        if baseline_budget.is_some() {
            ctx.param("baseline.truncated_tasks", run.baseline_truncated);
        }
        if real {
            ctx.param("failures", run.failure_kinds());
            let attempts = 2 * tasks.len();
            if attempts > 0 && run.failures("nugget") + run.failures("baseline") == attempts {
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

        let judge_summary = match (judge_mode, &model) {
            (JudgeMode::None, _) => None,
            (JudgeMode::Heuristic, _) => {
                run.judgements = run
                    .nugget
                    .iter()
                    .zip(&run.baseline)
                    .map(|(n, b)| judge::heuristic(n, b))
                    .collect();
                Some(judge::summarise(&run.judgements))
            }
            (JudgeMode::Model, Model::Real(r)) => {
                let mj = ModelJudge {
                    provider: r.provider.clone(),
                    timeout: r.timeout,
                    max_tokens: if r.max_output == DRAFT_MAX_TOKENS_THINKING {
                        DRAFT_MAX_TOKENS_THINKING
                    } else {
                        JUDGE_MAX_TOKENS
                    },
                };
                run.judgements = rt.block_on(judge_all(&world, &tasks, &run, &mj, ctx.seed()))?;
                Some(judge::summarise(&run.judgements))
            }
            (JudgeMode::Model, Model::Fake(_)) => None,
        };
        if let Some(j) = &judge_summary {
            ctx.param("judge.tasks_judged", j.judged);
            ctx.param("judge.forfeits", j.forfeits);
            ctx.param("judge.failure_kinds", json!(j.failure_kinds));
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
        if let Some(j) = &judge_summary {
            judge_metrics(j, judge_mode, &mut out);
        }
        drop(rt);
        // Stops local servers, if any (outside any async context).
        drop(model);
        Ok(out)
    }
}

/// What `judge` says in the report.
fn judge_description(mode: JudgeMode, model_desc: &str) -> String {
    match mode {
        JudgeMode::None => "none".into(),
        JudgeMode::Heuristic => "heuristic: deterministic pairwise scorer over the suite's scores \
             (voice: voice match; factuality: gold recall - hallucination; winner: voice + recall \
             - hallucination - leak), both orders"
            .into(),
        JudgeMode::Model => format!(
            "model: the same model as the drafts ({model_desc}), pairwise, both orders (Nugget as \
             A then as B), JSON verdict {{voice, factuality, winner}}, thread/context/drafts \
             fenced as untrusted"
        ),
    }
}

/// Run the model judge over every task.
async fn judge_all(
    world: &World,
    tasks: &[usize],
    run: &DraftRun,
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
            (&run.nugget_drafts[i], &run.nugget[i]),
            (&run.baseline_drafts[i], &run.baseline[i]),
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
    for arm in ["nugget", "baseline"] {
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

fn judge_metrics(j: &JudgeSummary, mode: JudgeMode, out: &mut Vec<Metric>) {
    let who = match mode {
        JudgeMode::Model => "model judge",
        _ => "heuristic judge",
    };
    let m = |name: &str, v: f64, dir: Direction, desc: String| {
        Metric::new(format!("draft.judge.{name}"), v, "ratio", dir, desc)
    };
    out.push(m(
        "nugget_win_rate",
        j.nugget_win_rate,
        Direction::HigherIsBetter,
        format!(
            "Pairwise {who}: share of judged tasks where both orders prefer the Nugget draft \
             (or the baseline request failed)"
        ),
    ));
    out.push(m(
        "baseline_win_rate",
        j.baseline_win_rate,
        Direction::LowerIsBetter,
        format!(
            "Pairwise {who}: share of judged tasks where both orders prefer the baseline draft"
        ),
    ));
    out.push(m(
        "tie_rate",
        j.tie_rate,
        Direction::Info,
        format!("Pairwise {who}: share of judged tasks where both orders call a tie"),
    ));
    out.push(m(
        "inconsistent_rate",
        j.inconsistent_rate,
        Direction::LowerIsBetter,
        format!(
            "Pairwise {who}: share of judged tasks where the A/B and B/A verdicts disagree \
             (position bias)"
        ),
    ));
    out.push(m(
        "voice.nugget_win_rate",
        j.voice_nugget_win_rate,
        Direction::Info,
        format!("Pairwise {who}, voice criterion: share where both orders prefer Nugget"),
    ));
    out.push(m(
        "factuality.nugget_win_rate",
        j.factuality_nugget_win_rate,
        Direction::Info,
        format!("Pairwise {who}, factuality criterion: share where both orders prefer Nugget"),
    ));
    out.push(Metric::new(
        "draft.judge.failures",
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
        for (arm, s, d) in [
            ("nugget", &run.nugget[i], &run.nugget_drafts[i]),
            ("baseline", &run.baseline[i], &run.baseline_drafts[i]),
        ] {
            let line = json!({
                "task": t.id,
                "arm": arm,
                "tags": t.tags,
                "draft": d,
                "reference": t.reference,
                "voice_match": s.voice_match,
                "gold_hit": s.gold_hit,
                "gold_total": s.gold_total,
                "hallucinated": s.hallucinated,
                "leaked": s.leaked,
                "prompt_exposed": s.prompt_exposed,
                "injection_resisted": s.injection_resisted,
                "prompt_tokens": s.prompt_tokens,
                "ttft_s": s.ttft.as_secs_f64(),
                "total_s": s.total.as_secs_f64(),
            });
            let mut line = line;
            let err = if arm == "nugget" {
                run.nugget_errors.get(i)
            } else {
                run.baseline_errors.get(i)
            };
            if let Some(Some(e)) = err {
                line["error"] = json!({ "kind": e.kind, "message": e.message });
            }
            if let Some(j) = run.judgements.get(i) {
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
    let label = if arm == "nugget" {
        "Nugget arm"
    } else {
        "Sentient-style baseline arm"
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
        assert_eq!(r.config.params["tasks"], 66);
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
        assert_eq!(text.lines().count(), 2 * 66);
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["task"], "maya-t01");
        assert!(first["draft"].as_str().is_some_and(|d| !d.is_empty()));
    }

    #[test]
    fn spread_picks_evenly() {
        assert_eq!(spread(10, 3), [0, 3, 6]);
        assert_eq!(spread(3, 5), [0, 1, 2]);
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
        assert_eq!(reqs.len(), 12);
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
        // No judge by default with --provider-url: 6 tasks x 2 arms.
        assert_eq!(reqs.len(), 12);
        let p = &r.config.params;
        assert_eq!(p["baseline.context_tokens"], 4096);
        assert_eq!(p["baseline.context_tokens_source"], "draft_context_tokens");
        let budget = prompt_budget(4096, DRAFT_MAX_TOKENS) as f64;
        assert_eq!(p["baseline.prompt_budget_tokens"], budget as u64);
        assert_eq!(value(&r, "draft.baseline.truncated_tasks"), 6.0);
        assert!(value(&r, "draft.baseline.prompt_tokens.p95") <= budget);
        assert_eq!(value(&r, "draft.nugget.failures"), 0.0);
        assert_eq!(value(&r, "draft.baseline.failures"), 0.0);
        assert_eq!(p["judge"], "none");
    }
}
