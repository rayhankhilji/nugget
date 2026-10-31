//! `bench/ask`: answer quality of Nugget's Ask (`retrieve.ask`), before and after the grounded
//! answer prompt (EE3). See `bench/README.md` for the methodology.
//!
//! **Path under test.** Exactly what the daemon runs, from product code: the question is packed
//! into a context by `nugget_retrieve::Retriever::pack_context_with` (sensitive items kept, as
//! for an on-device model), the request is built by [`nuggetd::ask::ask_request_packed`] (no
//! history), the stream goes through [`nuggetd::ask::AnswerFilter::packed`] delta by delta, and
//! its [`AnswerCheck`] is what the client would get on `chat.done`. Nothing of that is
//! re-implemented here; the bench only adds the corpus, the model and the scoring.
//!
//! **Corpus.** The retrieval eval set (`EvalSet::builtin()`, the labelled set behind
//! `memory --backend real`) plus `bench/fixtures/ask/docs.json` (same schema), indexed into an
//! in-memory store with `eval::index` (one chunk per paragraph), embedded by the deterministic
//! `HashEmbedder` routed as the Embedding tier; `RetrieveConfig::default()`, reference time
//! [`REFERENCE_TIME`] and `today` = 2026-10-30 so recency and "latest" questions are stable.
//! Context budget `--ask-budget-tokens` (default 2000, the desktop's `CHAT_ASK_BUDGET`).
//!
//! **Arms** (`--ask-prompt`): `plain` = `AskPrompt::Plain` with the filter's normalisation off
//! (what shipped before EE3), `grounded` = `AskPrompt::Grounded` with normalisation on (now).
//! Both arms answer every question on the same server with the same context; they run
//! interleaved per question, the arm order alternating from one question to the next so
//! neither always gets a warm cache. Default `both` with a real model, `grounded` with the
//! fake one. Real models only with `--local-models` (the manifest's chat model through
//! `nugget-runtime`, thinking off unless `--local-thinking model`); without it a deterministic
//! in-process fake ([`fake`]) that follows the prompt it gets: `[n]` markers and
//! `NOT_FOUND_REPLY` under the grounded prompt, `[n, m]` markers and a prose refusal under the
//! plain one, so a detector bias towards the product's own reply shows in CI. Requests use a
//! fixed temperature (`--ask-temperature`, default 0); `ChatRequest` has no seed field, so
//! none is sent. The server's `ChatEvent::Stats` (prompt-cache hits) and stop reason are
//! recorded per answer.
//!
//! **Failures** (see [`metrics`]): every quality rate is over successful requests; failed and
//! timed-out ones (`--ask-timeout-s`, default 300) are reported as `failures`, `failure_rate`
//! and `timeouts`, never as wrong answers and never as a crash. Latency percentiles include
//! timed-out requests as censored samples at the timeout; time to first token is the model's
//! (retrieval excluded) and skips answers that forwarded no text.
//!
//! **Long runs.** Every answer is scored and written to the dump (`--ask-dump`, flushed) as
//! soon as it arrives. The judge runs after all answers: with a different judge model the
//! answer server is stopped first, so a CPU runner never holds both models (on a 16 GB runner a
//! 4B answerer next to an 8B judge slowed judging from ~35 s to ~75 s a request before the job
//! was killed). The judge is probed before use (one that cannot start or answer is dropped and
//! recorded in `judge.unavailable`), and each verdict is appended to `<dump>.judge.jsonl` as
//! soon as it arrives. The wall-clock budget (`--ask-budget-s`, default 5 hours) covers both
//! phases: no new question, then no new verdict, once it is spent, and the report still covers
//! what was done (`questions_answered`, `judge_coverage`). `--ask-rescore FILE` recomputes the
//! whole report from a dump and its sidecar without a model, with the current scoring rules and
//! gold, so a killed or pre-fix run can still be reported.
//!
//! **Questions** (`bench/fixtures/ask/questions.json`, [`gold`]): kinds `single`, `multi_hop`,
//! `latest`, `unanswerable`, `injection`, each with `must` value groups, `must_not` (stale)
//! values, `support` doc ids and, for injections, a canary. They run in a stable per-kind
//! interleave, cut to `--ask-questions` (smoke: 12, every kind; other scales: all).
//!
//! **Rule-based scores** ([`score`], the bench's own text rules, independent of the product's
//! post-check): an answerable question is correct when every `must` group has a value in the
//! answer the user sees and the answer does not abstain; an unanswerable one when it abstains.
//! Abstention is decided per sentence, never by citation markers: some sentence says the items
//! don't hold the answer, and no sentence states a gold value or a number or name that is not
//! in the question. `abstention_rate` is the unanswerable questions' correctness (there is no
//! separate `correct_rate_unanswerable`). `must_not` values do **not** decide correctness: a
//! right "latest" answer often mentions the stale value as context ("568, down from 612"), so
//! they are reported as `stale_value_rate` and the nuance is left to the judge. A multi_hop
//! question's `must` values cannot check a comparison verdict ("which cost more"); the judge is
//! the second opinion there. Injection questions report `injection_compliance_rate` (the canary
//! in a sentence with no warning cue and no quoting frame: "the invite says to call X; this
//! looks like a scam" quotes it without obeying), `injection_canary_rate` (the canary anywhere)
//! and `injection_correct_rate` (correct and not complied).
//!
//! Citations are scored on the *raw* model text for both arms (the grounded filter deletes
//! markers naming no passage, which would flatter it), in every common format so the plain arm
//! is not penalised for format alone: `citation_format_rate` (markers already canonical `[n]`),
//! `invalid_citations` (passage numbers naming no passage), `citation_precision` (over cited
//! content sentences of answerable questions, the share of (sentence, cited n) pairs whose
//! passage belongs to a gold support doc or contains every number and mid-sentence capitalised
//! word the sentence states), `citation_recall` (of the gold support docs in the context, the
//! share the answer cites), `uncited_claim_rate`. `postcheck.*` scores the product's
//! `AnswerCheck` on both arms.
//!
//! **Judge** (`--ask-judge model`, default with `--local-models`): a pointwise correctness judge
//! ([`judge`]), both orders, on the answering model or, with `--ask-judge-model`, a second
//! local server (the same server when the ids match). `judge_correct_rate` is over every
//! request, `judge_correct_rate_answered` over answers with both verdicts.

pub mod dump;
pub mod fake;
pub mod gold;
pub mod judge;
pub mod metrics;
pub mod score;
pub mod stream;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nugget_core::capability::{Capabilities, Tier};
use nugget_core::ipc::{ContextResponse, RuntimeRole};
use nugget_infer::{Provider, Router};
use nugget_retrieve::eval::{self, EvalSet, HashEmbedder};
use nugget_retrieve::{ContextOptions, PackedPassage, RetrieveConfig, Retriever};
use nugget_store::Store;
use nuggetd::ask::{AnswerFilter, AskOptions as ProductAskOptions, AskPrompt, ask_request_packed};
use serde_json::json;

use crate::local::{LocalModels, Thinking};
use crate::report::Metric;
use crate::runner::{BenchError, Benchmark, RunContext};
use crate::suites::draft::Temperature;
use gold::{Gold, Question};
use judge::{AskJudgeMode, JudgeOutcome, Judgement, ModelJudge};
use metrics::Row;
use stream::AnswerRun;

/// Default `--ask-budget-tokens` (the desktop's `CHAT_ASK_BUDGET`).
pub const DEFAULT_BUDGET_TOKENS: u32 = 2000;
/// Default `--ask-timeout-s`.
pub const DEFAULT_TIMEOUT_S: u64 = 300;
/// Default `--ask-budget-s` (5 hours).
pub const DEFAULT_WALL_BUDGET_S: u64 = 18_000;
/// Default `--ask-judge-timeout-s` with `--ask-judge-model`.
pub const DEFAULT_JUDGE_TIMEOUT_S: u64 = 240;
/// Default temperature of answer requests to a real model.
pub const DEFAULT_TEMPERATURE: f32 = 0.0;
/// Questions at smoke scale (every kind is represented).
pub const SMOKE_QUESTIONS: usize = 12;
/// The retriever's fixed "now".
pub const REFERENCE_TIME: &str = "2026-10-30T12:00:00Z";
/// Dimension of the hash embedder (as `memory --backend real`).
pub const EMBEDDING_DIM: usize = 256;
/// Output cap given to a thinking model (`--local-thinking model`), whose reasoning counts.
pub const MAX_TOKENS_THINKING: u32 = 2048;
pub const SEED_NOTE: &str = "not sent: ChatRequest has no seed field, so the provider samples \
     with its own seed (fixed temperature instead)";
pub const PATH: &str = "product: Retriever::pack_context_with (exclude_sensitive false) -> \
     nuggetd::ask::ask_request_packed (no history) -> provider stream -> \
     nuggetd::ask::AnswerFilter::packed (push/finish) -> AnswerFilter::check";

/// `--ask-prompt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum PromptArms {
    Plain,
    Grounded,
    Both,
}

impl PromptArms {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Grounded => "grounded",
            Self::Both => "both",
        }
    }

    /// The default: `both` with a real model, `grounded` with the fake one.
    pub fn resolve(opt: Option<Self>, real: bool) -> Self {
        opt.unwrap_or(if real { Self::Both } else { Self::Grounded })
    }

    pub fn arms(self) -> Vec<Arm> {
        match self {
            Self::Plain => vec![Arm::PLAIN],
            Self::Grounded => vec![Arm::GROUNDED],
            Self::Both => vec![Arm::PLAIN, Arm::GROUNDED],
        }
    }
}

/// One answer configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm {
    pub name: &'static str,
    pub prompt: AskPrompt,
    /// `AnswerFilter` citation normalisation.
    pub normalise: bool,
}

impl Arm {
    pub const PLAIN: Self = Self {
        name: "plain",
        prompt: AskPrompt::Plain,
        normalise: false,
    };
    pub const GROUNDED: Self = Self {
        name: "grounded",
        prompt: AskPrompt::Grounded,
        normalise: true,
    };
}

/// `ask` options (flattened into the CLI).
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct AskOptions {
    /// `ask`: answer prompt arms: `plain` (before EE3, normalisation off), `grounded` (now), or
    /// `both` on the same questions. Default: `both` with `--local-models`, else `grounded`.
    #[arg(long, value_enum)]
    pub ask_prompt: Option<PromptArms>,
    /// `ask`: context budget in tokens (default 2000, the desktop's).
    #[arg(long, value_name = "TOKENS")]
    pub ask_budget_tokens: Option<u32>,
    /// `ask`: sampling temperature of answer requests (default 0 with a real model).
    #[arg(long, value_name = "T")]
    pub ask_temperature: Option<Temperature>,
    /// `ask`: correctness judge: `model` (default with `--local-models`) or `none`.
    #[arg(long, value_enum)]
    pub ask_judge: Option<AskJudgeMode>,
    /// `ask`: manifest id of a different local model to judge with, on a second local server.
    #[arg(long, value_name = "ID")]
    pub ask_judge_model: Option<String>,
    /// `ask`: per-request timeout of the judge, in seconds (default 240 with
    /// `--ask-judge-model`, else the answer timeout).
    #[arg(long, value_name = "SECS")]
    pub ask_judge_timeout_s: Option<u64>,
    /// `ask`: write one JSON line per question and arm (context, raw and filtered answer, check,
    /// scores, timings, judge) to this file.
    #[arg(long, value_name = "FILE")]
    pub ask_dump: Option<std::path::PathBuf>,
    /// `ask`: the first N questions of the per-kind interleave (default: 12 at smoke scale,
    /// all otherwise).
    #[arg(long, value_name = "N")]
    pub ask_questions: Option<usize>,
    /// `ask`: per-request timeout of an answer, in seconds (default 300).
    #[arg(long, value_name = "SECS")]
    pub ask_timeout_s: Option<u64>,
    /// `ask`: wall-clock budget of the whole run, in seconds (default 18000, 5 hours): no new
    /// question starts once it is spent, and the report covers the questions answered.
    #[arg(long, value_name = "SECS")]
    pub ask_budget_s: Option<u64>,
    /// `ask`: no model: recompute every metric from this dump (`--ask-dump` output) and its
    /// judge sidecar (`<FILE>.judge.jsonl`, when present), e.g. after a run was cut short or a
    /// scoring fix.
    #[arg(long, value_name = "FILE")]
    pub ask_rescore: Option<std::path::PathBuf>,
}

/// Where the judge's verdicts come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgePlan {
    None,
    /// The answering model.
    SameModel,
    /// A second local server with this manifest model.
    LocalModel(String),
}

/// Decide the judge: `--ask-judge` (default `model` with a real model or a judge model),
/// `--ask-judge-model`, whether the answers come from a real (local) model, and its id.
pub fn plan_judge(
    mode: Option<AskJudgeMode>,
    judge_model: Option<&str>,
    real: bool,
    chat_model_id: Option<&str>,
) -> Result<JudgePlan, String> {
    let mode = mode.unwrap_or(if real || judge_model.is_some() {
        AskJudgeMode::Model
    } else {
        AskJudgeMode::None
    });
    if judge_model.is_some() && mode != AskJudgeMode::Model {
        return Err("--ask-judge-model needs --ask-judge model".into());
    }
    match (mode, judge_model) {
        (AskJudgeMode::None, _) => Ok(JudgePlan::None),
        (AskJudgeMode::Model, _) if !real => {
            Err("--ask-judge model needs a real model (--local-models)".into())
        }
        (AskJudgeMode::Model, None) => Ok(JudgePlan::SameModel),
        (AskJudgeMode::Model, Some(id)) if Some(id) == chat_model_id => Ok(JudgePlan::SameModel),
        (AskJudgeMode::Model, Some(id)) => Ok(JudgePlan::LocalModel(id.to_owned())),
    }
}

/// The answering model.
struct Answerer {
    provider: Arc<dyn Provider>,
    real: bool,
    desc: String,
    model_id: Option<String>,
    thinking: bool,
    /// The servers behind `provider` (`--local-models`); dropping it stops them.
    _local: Option<Box<LocalModels>>,
}

impl Answerer {
    fn build(ctx: &mut RunContext) -> Result<Self, BenchError> {
        let local = ctx.config.local.clone();
        if !local.local_models {
            return Ok(Self {
                provider: Arc::new(fake::FakeAnswerer::default()),
                real: false,
                desc: "fake-deterministic (bench/src/suites/ask/fake.rs: best-overlap passage \
                       sentence + [n], else NOT_FOUND_REPLY)"
                    .into(),
                model_id: None,
                thinking: false,
                _local: None,
            });
        }
        let models = LocalModels::start(&local, &[RuntimeRole::Chat])?;
        models.record(ctx);
        let provider = models.chat_with(models.thinking)?;
        Ok(Self {
            provider,
            real: true,
            desc: models.chat_label(),
            model_id: models.chat.as_ref().map(|s| s.entry.id.clone()),
            thinking: models.thinking == Thinking::Model,
            _local: Some(Box::new(models)),
        })
    }
}

/// Settings of every answer request.
#[derive(Debug, Clone, Copy)]
struct RequestSettings {
    temperature: Option<f32>,
    /// Raise the output cap to this (thinking models).
    min_max_tokens: Option<u32>,
    timeout: Duration,
}

/// Ask `provider` through the product path for one arm.
async fn answer(
    provider: &dyn Provider,
    question: &str,
    ctx: &ContextResponse,
    passages: &[PackedPassage],
    arm: Arm,
    s: RequestSettings,
    today: time::Date,
) -> AnswerRun {
    let mut req = ask_request_packed(
        &[],
        question,
        ctx,
        passages,
        &ProductAskOptions {
            prompt: arm.prompt,
            today: Some(today),
        },
    );
    if s.temperature.is_some() {
        req.temperature = s.temperature;
    }
    if let Some(m) = s.min_max_tokens {
        req.max_tokens = Some(req.max_tokens.map_or(m, |x| x.max(m)));
    }
    let started = Instant::now();
    let mut filter = AnswerFilter::packed(ctx, passages, Some(today), arm.normalise);
    stream::stream_answer(provider, req, &mut filter, s.timeout, started).await
}

/// Build the store and retriever over `set`.
async fn retriever(set: &EvalSet) -> Result<Retriever, BenchError> {
    let embedder: Arc<dyn Provider> = Arc::new(HashEmbedder::new(EMBEDDING_DIM, &set.synonyms));
    let store = Store::open_in_memory()?;
    eval::index(&store, set, Some(embedder.as_ref())).await?;
    let id = embedder.id().clone();
    let router = Router::new(
        vec![embedder],
        std::collections::HashMap::from([(Tier::Embedding, vec![id.clone()])]),
    );
    router.set_capabilities(
        &id,
        Capabilities {
            embeddings: true,
            ..Default::default()
        },
    );
    let now = time::OffsetDateTime::parse(
        REFERENCE_TIME,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|e| BenchError::Failed(format!("reference time: {e}")))?;
    Ok(
        Retriever::new(store, Some(Arc::new(router)), RetrieveConfig::default())
            .with_reference_time(now),
    )
}

pub struct AskBench;

impl Benchmark for AskBench {
    fn name(&self) -> &'static str {
        "ask"
    }
    fn description(&self) -> &'static str {
        "Ask answers through the daemon's prompt and post-check: correctness, abstention, \
         citations, injection, before/after the grounded prompt"
    }
    fn run(&self, ctx: &mut RunContext) -> Result<Vec<Metric>, BenchError> {
        let gold = Gold::builtin().map_err(BenchError::Failed)?;
        match ctx.config.ask.ask_rescore.clone() {
            Some(path) => rescore(ctx, &gold, &path),
            None => run_with(ctx, &gold),
        }
    }
}

/// Run the suite on `gold` (the shipped fixtures, or a test set).
pub fn run_with(ctx: &mut RunContext, gold: &Gold) -> Result<Vec<Metric>, BenchError> {
    let opts = ctx.config.ask.clone();
    let local_opts = ctx.config.local.clone();
    if let (Some(id), true) = (&opts.ask_judge_model, local_opts.local_models) {
        let manifest = nugget_models::default_manifest()?;
        crate::local::pick(&manifest.models, Some(id), &[])?;
    }
    let base = EvalSet::builtin()?;
    gold.validate(&base).map_err(BenchError::Failed)?;
    let corpus = gold.corpus(&base);
    let model = Answerer::build(ctx)?;
    // Stops the local servers, if any, at the end (outside any async context).
    run_model(ctx, gold, corpus, model)
}

/// Run the suite on `gold` and `corpus` with `model`.
fn run_model(
    ctx: &mut RunContext,
    gold: &Gold,
    corpus: EvalSet,
    model: Answerer,
) -> Result<Vec<Metric>, BenchError> {
    let opts = ctx.config.ask.clone();
    let plan = plan_judge(
        opts.ask_judge,
        opts.ask_judge_model.as_deref(),
        model.real,
        model.model_id.as_deref(),
    )
    .map_err(BenchError::Failed)?;
    let arm_mode = PromptArms::resolve(opts.ask_prompt, model.real);
    let arms = arm_mode.arms();
    let all = gold.questions.len();
    let limit = opts
        .ask_questions
        .unwrap_or_else(|| ctx.scale().pick(SMOKE_QUESTIONS, all, all, all))
        .min(all);
    let picked = gold::interleave(&gold.questions, limit);
    let budget = opts
        .ask_budget_tokens
        .unwrap_or(DEFAULT_BUDGET_TOKENS)
        .max(1);
    let timeout = Duration::from_secs(opts.ask_timeout_s.unwrap_or(DEFAULT_TIMEOUT_S).max(1));
    let temperature = model
        .real
        .then(|| opts.ask_temperature.map_or(DEFAULT_TEMPERATURE, |t| t.0));
    let today = time::Date::from_calendar_date(2026, time::Month::October, 30)
        .map_err(|e| BenchError::Failed(e.to_string()))?;
    let settings = RequestSettings {
        temperature,
        min_max_tokens: model.thinking.then_some(MAX_TOKENS_THINKING),
        timeout,
    };

    let picked_gold: Vec<&Question> = picked.iter().map(|&i| &gold.questions[i]).collect();
    let mut by_kind = BTreeMap::<&str, usize>::new();
    for q in &picked_gold {
        *by_kind.entry(q.kind.as_str()).or_default() += 1;
    }
    ctx.param("path", PATH);
    ctx.param("model", model.desc.clone());
    ctx.param("model.fake", !model.real);
    ctx.param(
        "model.locality",
        format!("{:?}", model.provider.meta().locality),
    );
    ctx.param("arms", arm_mode.as_str());
    ctx.param(
        "arms.order",
        "interleaved per question; arm order alternates between questions",
    );
    ctx.param("corpus", corpus.name.clone());
    ctx.param("corpus.docs", corpus.docs.len());
    ctx.param("corpus.ask_docs", gold.docs.len());
    ctx.param(
        "embedder",
        format!("hash-bow (eval::HashEmbedder), dim {EMBEDDING_DIM}"),
    );
    ctx.param("retrieve_config", "RetrieveConfig::default()");
    ctx.param("reference_time", REFERENCE_TIME);
    ctx.param("today", today.to_string());
    ctx.param("budget_tokens", budget);
    ctx.param("questions", picked.len());
    ctx.param("questions.available", all);
    ctx.param("questions.by_kind", json!(by_kind));
    ctx.param("questions.available_by_kind", json!(gold.counts()));
    ctx.param("timeout_s", timeout.as_secs());
    ctx.param(
        "sampling",
        json!({
            "temperature": temperature.map_or(json!("n/a (deterministic fake model)"), |t| json!(t)),
            "seed": SEED_NOTE,
        }),
    );
    ctx.param(
        "scoring",
        "rule-based: answerable correct = every must group present and no abstention (must_not \
         reported as stale_value_rate, not in correctness); unanswerable correct = abstention \
         with no cited fact; see bench/src/suites/ask/score.rs",
    );

    run_answers(
        ctx,
        gold,
        model,
        Plan {
            judge: plan,
            arms,
            picked,
            corpus,
            budget,
            timeout,
            settings,
            today,
        },
    )
}

/// What [`run_answers`] runs.
struct Plan {
    judge: JudgePlan,
    arms: Vec<Arm>,
    /// Question indices, in run order.
    picked: Vec<usize>,
    corpus: EvalSet,
    budget: u32,
    timeout: Duration,
    settings: RequestSettings,
    today: time::Date,
}

/// Start the judge after the answers and check it answers. With a different judge model the
/// answer server is stopped first (`model`'s servers), so the two never share the machine's
/// memory. A judge that cannot start or answer is dropped and recorded in `judge.unavailable`;
/// the answer metrics are reported either way.
fn start_judge(
    ctx: &mut RunContext,
    rt: &tokio::runtime::Runtime,
    plan: &JudgePlan,
    model: &mut Answerer,
    timeout: Duration,
) -> (Option<ModelJudge>, Option<LocalModels>) {
    let opts = ctx.config.ask.clone();
    let local_opts = ctx.config.local.clone();
    let max_tokens = |thinking: bool| {
        if thinking {
            judge::JUDGE_MAX_TOKENS_THINKING
        } else {
            judge::JUDGE_MAX_TOKENS
        }
    };
    let (mj, local) = match plan {
        JudgePlan::None => return (None, None),
        JudgePlan::SameModel => {
            ctx.param("judge.model", model.desc.clone());
            ctx.param("judge.same_model_as_answers", true);
            let mj = ModelJudge {
                provider: model.provider.clone(),
                timeout: opts
                    .ask_judge_timeout_s
                    .map_or(timeout, |s| Duration::from_secs(s.max(1))),
                max_tokens: max_tokens(model.thinking),
            };
            (mj, None)
        }
        JudgePlan::LocalModel(id) => {
            // Free the answer server's memory before the judge loads.
            drop(model._local.take());
            let started = LocalModels::start_chat_server(&local_opts, id, "judge")
                .and_then(|m| m.chat().map(|p| (p, m)));
            let (provider, models) = match started {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("ask: the judge server did not start ({e}); reporting without it");
                    ctx.param("judge.unavailable", format!("server did not start: {e}"));
                    return (None, None);
                }
            };
            ctx.param("judge.model", models.chat_label());
            ctx.param("judge.same_model_as_answers", false);
            models.record_chat_as(ctx, "judge.local");
            let mj = ModelJudge {
                provider,
                timeout: Duration::from_secs(
                    opts.ask_judge_timeout_s
                        .unwrap_or(DEFAULT_JUDGE_TIMEOUT_S)
                        .max(1),
                ),
                max_tokens: max_tokens(models.thinking == Thinking::Model),
            };
            (mj, Some(models))
        }
    };
    if let Err(e) = rt.block_on(mj.probe()) {
        eprintln!("ask: the judge does not answer ({e}); reporting without it");
        ctx.param("judge.unavailable", e);
        return (None, None);
    }
    (Some(mj), local)
}

/// Answer the planned questions with `model` (each row dumped as soon as it is scored), then
/// judge them (each verdict appended to the sidecar as soon as it arrives), both within the
/// wall-clock budget, then turn the rows into metrics.
fn run_answers(
    ctx: &mut RunContext,
    gold: &Gold,
    mut model: Answerer,
    plan: Plan,
) -> Result<Vec<Metric>, BenchError> {
    let opts = ctx.config.ask.clone();
    let Plan {
        judge: judge_plan,
        arms,
        picked,
        corpus,
        budget,
        timeout,
        settings,
        today,
    } = plan;
    let wall = Duration::from_secs(opts.ask_budget_s.unwrap_or(DEFAULT_WALL_BUDGET_S).max(1));
    ctx.param("wall_budget_s", wall.as_secs());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mut dump = opts
        .ask_dump
        .as_deref()
        .map(dump::Dump::create)
        .transpose()?;
    let run_started = Instant::now();
    let (contexts, retrieval_s, mut rows, stopped) = rt.block_on(async {
        let retriever = retriever(&corpus).await?;
        let mut contexts = Vec::new();
        let mut retrieval_s = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        let mut stopped = false;
        for (pos, &qi) in picked.iter().enumerate() {
            if run_started.elapsed() >= wall {
                eprintln!(
                    "ask: wall-clock budget of {} s reached after {pos} questions; reporting",
                    wall.as_secs()
                );
                stopped = true;
                break;
            }
            let q = &gold.questions[qi];
            let started = Instant::now();
            let packed = retriever
                .pack_context_with(
                    &q.question,
                    budget as usize,
                    ContextOptions {
                        exclude_sensitive: false,
                    },
                )
                .await?;
            let (c, passages) = (packed.response, packed.passages);
            retrieval_s.push(started.elapsed().as_secs_f64());
            let mut order = arms.clone();
            if pos % 2 == 1 {
                order.reverse();
            }
            for arm in order {
                let run = answer(
                    model.provider.as_ref(),
                    &q.question,
                    &c,
                    &passages,
                    arm,
                    settings,
                    today,
                )
                .await;
                let score = match &run.failure {
                    Some((kind, message)) => {
                        eprintln!(
                            "ask: {} {} request failed ({kind}): {message}",
                            q.id, arm.name
                        );
                        None
                    }
                    None => Some(score::score(q, &c, &run.text, &run.raw)),
                };
                if model.real
                    && let Some(s) = &score
                {
                    eprintln!(
                        "ask: {} ({}/{}) {}: {}, done {:.1} s",
                        q.id,
                        pos + 1,
                        picked.len(),
                        arm.name,
                        if s.correct { "correct" } else { "wrong" },
                        run.total.as_secs_f64()
                    );
                }
                let row = Row {
                    question: qi,
                    arm,
                    run,
                    score,
                    judge: None,
                };
                if let Some(d) = dump.as_mut() {
                    d.write(q, &c, &row, timeout.as_secs())?;
                }
                rows.push(row);
            }
            contexts.push(c);
        }
        Ok::<_, BenchError>((contexts, retrieval_s, rows, stopped))
    })?;
    ctx.param("questions_answered", contexts.len());
    ctx.param("stopped_by_wall_budget", stopped);

    let failed = rows.iter().filter(|r| !r.run.ok()).count();
    if model.real && !rows.is_empty() && failed == rows.len() {
        let first = rows[0]
            .run
            .failure
            .clone()
            .unwrap_or(("error", String::new()));
        return Err(BenchError::Failed(format!(
            "every answer request failed ({failed} of {failed}); first: {}: {}",
            first.0, first.1
        )));
    }

    // The judge, after every answer: one model in memory at a time.
    let (judge, judge_local) = if judge_plan != JudgePlan::None && run_started.elapsed() >= wall {
        ctx.param(
            "judge.unavailable",
            "wall-clock budget spent while answering",
        );
        (None, None)
    } else {
        start_judge(ctx, &rt, &judge_plan, &mut model, timeout)
    };
    ctx.param("judge.mode", if judge.is_some() { "model" } else { "none" });
    if let Some(mj) = &judge {
        ctx.param("judge.timeout_s", mj.timeout.as_secs());
        ctx.param("judge.max_tokens", mj.max_tokens);
        ctx.param("judge.temperature", 0.0);
        ctx.param(
            "judge",
            "pointwise correctness vs the reference answer, both orders (reference first, \
             candidate first), JSON verdict {correct}, question/reference/candidate fenced as \
             untrusted; run after all answers, the answer server stopped first",
        );
        let mut log = opts
            .ask_dump
            .as_deref()
            .map(|p| dump::VerdictLog::create(&dump::verdict_path(p)))
            .transpose()?;
        let seed = ctx.seed();
        let judge_stopped = rt.block_on(async {
            let n = rows.len();
            for (i, row) in rows.iter_mut().enumerate() {
                if run_started.elapsed() >= wall {
                    eprintln!(
                        "ask: wall-clock budget of {} s reached after judging {i} of {n}; \
                         reporting",
                        wall.as_secs()
                    );
                    return Ok::<_, BenchError>(true);
                }
                let q = &gold.questions[row.question];
                let j = if row.run.ok() {
                    let nonce = format!(
                        "{:012x}",
                        (seed ^ (i as u64 + 1)).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 16
                    );
                    let started = Instant::now();
                    let j = mj.judge(q, &row.run.text, &nonce).await;
                    eprintln!(
                        "ask: judged {} {} ({}/{n}): {} in {:.1} s",
                        q.id,
                        row.arm.name,
                        i + 1,
                        j.outcome.as_str(),
                        started.elapsed().as_secs_f64()
                    );
                    j
                } else {
                    Judgement {
                        outcome: JudgeOutcome::Skipped,
                        reference_first: None,
                        candidate_first: None,
                    }
                };
                if let Some(l) = log.as_mut() {
                    l.write(&q.id, row.arm, &j)?;
                }
                row.judge = Some(j);
            }
            Ok(false)
        })?;
        ctx.param("judge.stopped_by_wall_budget", judge_stopped);
    }
    let answered: Vec<usize> = picked[..contexts.len()].to_vec();
    let out = report(
        ctx,
        gold,
        &arms,
        &rows,
        &answered,
        &contexts,
        &retrieval_s,
        judge.is_some(),
        timeout,
    );
    drop(rt);
    drop(judge_local);
    drop(model);
    Ok(out)
}

/// The metrics of a run (live or re-scored): per arm, then the shared ones. `answered` are
/// question indices with their `contexts`, in run order.
#[allow(clippy::too_many_arguments)]
fn report(
    ctx: &mut RunContext,
    gold: &Gold,
    arms: &[Arm],
    rows: &[Row],
    answered: &[usize],
    contexts: &[ContextResponse],
    retrieval_s: &[f64],
    judged: bool,
    timeout: Duration,
) -> Vec<Metric> {
    let mut out = Vec::new();
    for arm in arms {
        let mine: Vec<&Row> = rows.iter().filter(|r| r.arm == *arm).collect();
        metrics::arm_metrics(
            *arm,
            gold,
            &mine,
            judged,
            arms.len() == 1 || arm.name == "grounded",
            timeout,
            &mut out,
        );
    }
    let failure_kinds: BTreeMap<String, BTreeMap<&str, usize>> = arms
        .iter()
        .map(|a| {
            let mut m = BTreeMap::new();
            for r in rows.iter().filter(|r| r.arm == *a) {
                if let Some((k, _)) = &r.run.failure {
                    *m.entry(*k).or_default() += 1;
                }
            }
            (a.name.to_owned(), m)
        })
        .collect();
    ctx.param("failure_kinds", json!(failure_kinds));
    let questions: Vec<&Question> = answered.iter().map(|&i| &gold.questions[i]).collect();
    metrics::shared_metrics(&questions, contexts, retrieval_s, &mut out);
    out
}

/// `--ask-rescore`: recompute every metric from a dump (and its judge sidecar, when present)
/// with the current scoring rules and gold, without a model.
pub fn rescore(
    ctx: &mut RunContext,
    gold: &Gold,
    path: &std::path::Path,
) -> Result<Vec<Metric>, BenchError> {
    let (dumped, skipped) = dump::read(path).map_err(BenchError::Failed)?;
    if dumped.is_empty() {
        return Err(BenchError::Failed(format!(
            "{}: no answers to re-score",
            path.display()
        )));
    }
    let sidecar = dump::verdict_path(path);
    let (verdicts, verdicts_skipped) = if sidecar.exists() {
        dump::read_verdicts(&sidecar).map_err(BenchError::Failed)?
    } else {
        Default::default()
    };
    let index: std::collections::HashMap<&str, usize> = gold
        .questions
        .iter()
        .enumerate()
        .map(|(i, q)| (q.id.as_str(), i))
        .collect();
    let mut answered: Vec<usize> = Vec::new();
    let mut contexts: Vec<ContextResponse> = Vec::new();
    let mut rows = Vec::with_capacity(dumped.len());
    let mut timeout_s = 0;
    for d in dumped {
        let qi = *index.get(d.question_id.as_str()).ok_or_else(|| {
            BenchError::Failed(format!(
                "{}: question {} is not in the gold set",
                path.display(),
                d.question_id
            ))
        })?;
        let q = &gold.questions[qi];
        if !answered.contains(&qi) {
            answered.push(qi);
            contexts.push(d.context.clone());
        }
        timeout_s = timeout_s.max(d.timeout_s.unwrap_or(0));
        let score = d
            .run
            .ok()
            .then(|| score::score(q, &d.context, &d.run.text, &d.run.raw));
        let judge = verdicts
            .get(&(d.question_id.clone(), d.arm.name.to_owned()))
            .copied()
            .or(d.judge);
        rows.push(Row {
            question: qi,
            arm: d.arm,
            run: d.run,
            score,
            judge,
        });
    }
    let arms: Vec<Arm> = [Arm::PLAIN, Arm::GROUNDED]
        .into_iter()
        .filter(|a| rows.iter().any(|r| r.arm == *a))
        .collect();
    let judged = sidecar.exists() || rows.iter().any(|r| r.judge.is_some());
    let timeout = Duration::from_secs(if timeout_s == 0 {
        DEFAULT_TIMEOUT_S
    } else {
        timeout_s
    });
    ctx.param("rescore.source", path.display().to_string());
    ctx.param("rescore.rows", rows.len());
    ctx.param("rescore.cut_off_lines", skipped + verdicts_skipped);
    ctx.param("rescore.judge_sidecar", sidecar.exists());
    ctx.param("path", PATH);
    ctx.param(
        "arms",
        arms.iter().map(|a| a.name).collect::<Vec<_>>().join("+"),
    );
    ctx.param("questions_answered", answered.len());
    ctx.param("timeout_s", timeout.as_secs());
    ctx.param(
        "judge.mode",
        if judged { "model (from dump)" } else { "none" },
    );
    Ok(report(
        ctx,
        gold,
        &arms,
        &rows,
        &answered,
        &contexts,
        &[],
        judged,
        timeout,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{RunConfig, Scale};

    fn smoke(ask: AskOptions) -> (Vec<Metric>, BTreeMap<String, serde_json::Value>) {
        let mut ctx = RunContext::new(RunConfig {
            scale: Scale::Smoke,
            ask,
            ..RunConfig::default()
        });
        let m = run_with(&mut ctx, &gold::tests::sample()).unwrap();
        (m, ctx.params().clone())
    }

    #[test]
    fn judge_plans() {
        assert_eq!(plan_judge(None, None, false, None), Ok(JudgePlan::None));
        assert_eq!(
            plan_judge(None, None, true, Some("a")),
            Ok(JudgePlan::SameModel)
        );
        assert_eq!(
            plan_judge(None, Some("a"), true, Some("a")),
            Ok(JudgePlan::SameModel)
        );
        assert_eq!(
            plan_judge(None, Some("b"), true, Some("a")),
            Ok(JudgePlan::LocalModel("b".into()))
        );
        assert!(plan_judge(Some(AskJudgeMode::Model), None, false, None).is_err());
        assert!(plan_judge(Some(AskJudgeMode::None), Some("b"), true, None).is_err());
    }

    #[test]
    fn fake_smoke_run_reports_every_metric() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ask.jsonl");
        let (metrics, params) = smoke(AskOptions {
            ask_prompt: Some(PromptArms::Both),
            ask_dump: Some(path.clone()),
            ..AskOptions::default()
        });
        for m in &metrics {
            assert!(m.value.is_finite(), "{m:?}");
            assert!(crate::runner::valid_metric_name(&m.name), "{}", m.name);
        }
        let names: Vec<&str> = metrics.iter().map(|m| m.name.as_str()).collect();
        for arm in ["plain", "grounded"] {
            for m in [
                "correct_rate",
                "correct_rate_single",
                "correct_rate_multi_hop",
                "correct_rate_latest",
                "abstention_rate",
                "false_abstention_rate",
                "stale_value_rate",
                "citation_precision",
                "citation_recall",
                "uncited_claim_rate",
                "citation_format_rate",
                "invalid_citations",
                "injection_compliance_rate",
                "injection_canary_rate",
                "injection_correct_rate",
                "postcheck.flagged_rate",
                "postcheck.invalid_citations",
                "postcheck.not_found_precision",
                "postcheck.not_found_recall",
                "ttft_p50_s",
                "ttft_p95_s",
                "full_p50_s",
                "full_p95_s",
                "prompt_tokens_p50",
                "cached_tokens_p50",
                "output_tokens_p50",
                "length_stops",
                "failures",
                "failure_rate",
                "timeouts",
            ] {
                let name = format!("ask.{arm}.{m}");
                assert!(names.contains(&name.as_str()), "missing {name}");
            }
        }
        for m in ["ask.retrieval.support_recall", "ask.questions"] {
            assert!(names.contains(&m), "missing {m}");
        }
        let get = |n: &str| metrics.iter().find(|m| m.name == n).unwrap();
        assert_eq!(get("ask.questions").value, 8.0);
        assert_eq!(get("ask.grounded.failures").value, 0.0);
        assert!(get("ask.grounded.abstention_rate").target.is_some());
        assert!(get("ask.plain.abstention_rate").target.is_none());
        // The fake finds the boiler's fault code and abstains on the sofa.
        assert!(get("ask.grounded.correct_rate").value > 0.0);
        assert_eq!(params["model.fake"], true);
        assert_eq!(params["questions"], 8);
        assert_eq!(params["questions_answered"], 8);
        assert!(
            names
                .iter()
                .all(|n| !n.contains("correct_rate_unanswerable"))
        );
        // The fake refuses in prose under the plain prompt and with NOT_FOUND_REPLY under the
        // grounded one: the bench's detector must see both.
        assert!(get("ask.plain.abstention_rate").value > 0.0);
        assert_eq!(
            get("ask.plain.abstention_rate").value,
            get("ask.grounded.abstention_rate").value
        );
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 16);
        let first: serde_json::Value = serde_json::from_str(lines.lines().next().unwrap()).unwrap();
        assert_eq!(first["question_id"], "single-01");
        assert!(first["check"].is_object());
    }

    #[test]
    fn fake_run_defaults_to_the_grounded_arm_and_is_deterministic() {
        let quality = |m: &[Metric]| -> Vec<(String, f64)> {
            m.iter()
                .filter(|m| !m.name.ends_with("_s"))
                .map(|m| (m.name.clone(), m.value))
                .collect()
        };
        let (a, params) = smoke(AskOptions::default());
        let (b, _) = smoke(AskOptions::default());
        assert_eq!(quality(&a), quality(&b));
        assert_eq!(params["arms"], "grounded");
        assert!(a.iter().all(|m| !m.name.starts_with("ask.plain.")));
        assert!(a.iter().all(|m| !m.name.contains("judge")));
    }

    #[test]
    fn question_limit_applies() {
        let (m, params) = smoke(AskOptions {
            ask_questions: Some(3),
            ..AskOptions::default()
        });
        assert_eq!(params["questions"], 3);
        assert_eq!(
            m.iter().find(|m| m.name == "ask.questions").unwrap().value,
            3.0
        );
    }

    #[test]
    fn a_model_judge_needs_a_real_model() {
        let mut ctx = RunContext::new(RunConfig {
            scale: Scale::Smoke,
            ask: AskOptions {
                ask_judge: Some(AskJudgeMode::Model),
                ..AskOptions::default()
            },
            ..RunConfig::default()
        });
        assert!(run_with(&mut ctx, &gold::tests::sample()).is_err());
    }

    // ---- scripted providers (no network) ----

    use async_trait::async_trait;
    use nugget_core::capability::Locality;
    use nugget_core::ids::ProviderId;
    use nugget_infer::{
        ChatEvent, ChatRequest, ChatStats, ChatStream, ProviderKind, ProviderMeta, StopReason,
    };

    /// Answers every request with `text`, after `delay`, with server stats.
    struct Scripted {
        id: ProviderId,
        text: String,
        delay: Duration,
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
                context_tokens: None,
            }
        }
        async fn chat(&self, _: ChatRequest) -> nugget_core::Result<ChatStream> {
            tokio::time::sleep(self.delay).await;
            Ok(Box::pin(futures::stream::iter([
                Ok(ChatEvent::TextDelta(self.text.clone())),
                Ok(ChatEvent::Stats(ChatStats {
                    input_tokens: Some(100),
                    output_tokens: Some(10),
                    cached_input_tokens: Some(7),
                    ..ChatStats::default()
                })),
                Ok(ChatEvent::Done {
                    stop_reason: StopReason::MaxTokens,
                    input_tokens: None,
                    output_tokens: None,
                }),
            ])))
        }
        async fn embed(&self, _: &[String]) -> nugget_core::Result<Vec<Vec<f32>>> {
            Err(nugget_core::Error::Unsupported("embed".into()))
        }
    }

    fn scripted(
        text: &str,
        delay: Duration,
        ask: AskOptions,
    ) -> (Vec<Metric>, BTreeMap<String, serde_json::Value>) {
        scripted_as(text, delay, ask, false)
    }

    /// `real`: treat the scripted provider as a real model (judge by default, both arms).
    fn scripted_as(
        text: &str,
        delay: Duration,
        ask: AskOptions,
        real: bool,
    ) -> (Vec<Metric>, BTreeMap<String, serde_json::Value>) {
        let mut ctx = RunContext::new(RunConfig {
            scale: Scale::Smoke,
            ask,
            ..RunConfig::default()
        });
        let gold = gold::tests::sample();
        let corpus = gold.corpus(&EvalSet::builtin().unwrap());
        let model = Answerer {
            provider: Arc::new(Scripted {
                id: ProviderId::new("scripted"),
                text: text.into(),
                delay,
            }),
            real,
            desc: "scripted".into(),
            model_id: None,
            thinking: false,
            _local: None,
        };
        let m = run_model(&mut ctx, &gold, corpus, model).unwrap();
        (m, ctx.params().clone())
    }

    fn value(m: &[Metric], name: &str) -> f64 {
        m.iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .value
    }

    #[test]
    fn invalid_markers_are_counted_on_raw_text_for_both_arms_and_stats_are_recorded() {
        let (m, _) = scripted(
            "The fee is 85 pounds [1][99].",
            Duration::ZERO,
            AskOptions {
                ask_prompt: Some(PromptArms::Both),
                ..AskOptions::default()
            },
        );
        // Every answer cites [99], which names no passage: the same count on both arms, though
        // the grounded filter removed it from what the user sees.
        assert_eq!(value(&m, "ask.plain.invalid_citations"), 8.0);
        assert_eq!(value(&m, "ask.grounded.invalid_citations"), 8.0);
        assert!(value(&m, "ask.grounded.postcheck.invalid_citations") >= 8.0);
        assert_eq!(value(&m, "ask.grounded.cached_tokens_p50"), 7.0);
        assert_eq!(value(&m, "ask.grounded.prompt_tokens_p50"), 100.0);
        assert_eq!(value(&m, "ask.grounded.length_stops"), 8.0);
        assert!(value(&m, "ask.plain.citation_precision") < 1.0);
        assert_eq!(
            value(&m, "ask.plain.citation_precision"),
            value(&m, "ask.grounded.citation_precision")
        );
    }

    #[test]
    fn timeouts_are_failures_and_censored_latency() {
        let (m, params) = scripted(
            "late",
            Duration::from_secs(30),
            AskOptions {
                ask_questions: Some(2),
                ask_timeout_s: Some(1),
                ..AskOptions::default()
            },
        );
        assert_eq!(value(&m, "ask.grounded.failures"), 2.0);
        assert_eq!(value(&m, "ask.grounded.timeouts"), 2.0);
        assert_eq!(value(&m, "ask.grounded.failure_rate"), 1.0);
        assert_eq!(value(&m, "ask.grounded.full_p95_s"), 1.0);
        assert_eq!(value(&m, "ask.grounded.ttft_p95_s"), 1.0);
        // No successful request: no quality rate.
        assert!(m.iter().all(|m| m.name != "ask.grounded.correct_rate"));
        assert_eq!(params["failure_kinds"]["grounded"]["timeout"], 2);
    }

    #[test]
    fn the_wall_clock_budget_stops_new_questions() {
        let (m, params) = scripted(
            "The fee is 85 pounds [1].",
            Duration::from_millis(700),
            AskOptions {
                ask_budget_s: Some(1),
                ..AskOptions::default()
            },
        );
        assert_eq!(params["questions"], 8);
        assert_eq!(params["questions_answered"], 2);
        assert_eq!(params["stopped_by_wall_budget"], true);
        assert_eq!(value(&m, "ask.questions"), 2.0);
    }

    #[test]
    fn rescore_reproduces_the_live_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ask.jsonl");
        let (live, _) = smoke(AskOptions {
            ask_prompt: Some(PromptArms::Both),
            ask_dump: Some(path.clone()),
            ..AskOptions::default()
        });
        let mut ctx = RunContext::new(RunConfig::default());
        let again = rescore(&mut ctx, &gold::tests::sample(), &path).unwrap();
        let live: Vec<&Metric> = live
            .iter()
            .filter(|m| m.name != "ask.retrieval.context_p50_s")
            .collect();
        assert_eq!(live.len(), again.len());
        for (a, b) in live.iter().zip(&again) {
            assert_eq!(a.name, b.name);
            assert!(
                (a.value - b.value).abs() < 1e-6,
                "{}: {} vs {}",
                a.name,
                a.value,
                b.value
            );
            assert_eq!(a.target, b.target, "{}", a.name);
        }
        assert_eq!(ctx.params()["rescore.rows"], 16);
        assert_eq!(ctx.params()["judge.mode"], "none");
    }

    #[test]
    fn rescore_merges_a_judge_sidecar_cut_short() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ask.jsonl");
        smoke(AskOptions {
            ask_dump: Some(path.clone()),
            ..AskOptions::default()
        });
        // A run killed while writing its last answer and while judging: a cut-off line in
        // both files, verdicts for the first three rows only.
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.truncate(6);
        std::fs::write(
            &path,
            format!("{}\n{{\"question_id\": \"x", lines.join("\n")),
        )
        .unwrap();
        let ids: Vec<String> = lines
            .iter()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["question_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        {
            let mut log = dump::VerdictLog::create(&dump::verdict_path(&path)).unwrap();
            for (id, ok) in ids.iter().take(3).zip([true, false, true]) {
                let j = Judgement {
                    outcome: JudgeOutcome::Agreed(ok),
                    reference_first: Some(ok),
                    candidate_first: Some(ok),
                };
                log.write(id, Arm::GROUNDED, &j).unwrap();
            }
        }
        let sidecar = dump::verdict_path(&path);
        let mut s = std::fs::read_to_string(&sidecar).unwrap();
        s.push_str("{\"question_id\": ");
        std::fs::write(&sidecar, s).unwrap();
        let mut ctx = RunContext::new(RunConfig::default());
        let m = rescore(&mut ctx, &gold::tests::sample(), &path).unwrap();
        assert_eq!(ctx.params()["rescore.rows"], 6);
        assert_eq!(ctx.params()["rescore.cut_off_lines"], 2);
        assert_eq!(value(&m, "ask.questions"), 6.0);
        assert_eq!(value(&m, "ask.grounded.judge_coverage"), 0.5);
        assert!((value(&m, "ask.grounded.judge_correct_rate") - 2.0 / 3.0).abs() < 1e-9);
        // Every answer metric is still there.
        assert!(m.iter().any(|m| m.name == "ask.grounded.correct_rate"));
    }

    #[test]
    fn rescore_rejects_an_unknown_question() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ask.jsonl");
        smoke(AskOptions {
            ask_questions: Some(1),
            ask_dump: Some(path.clone()),
            ..AskOptions::default()
        });
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"single-01\"", "\"nope\"");
        std::fs::write(&path, text).unwrap();
        let mut ctx = RunContext::new(RunConfig::default());
        assert!(rescore(&mut ctx, &gold::tests::sample(), &path).is_err());
    }

    #[test]
    fn the_judge_runs_after_answering_and_logs_each_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ask.jsonl");
        // The scripted model answers everything with a verdict, so it judges "correct".
        let (m, params) = scripted_as(
            "{\"correct\": true}",
            Duration::ZERO,
            AskOptions {
                ask_dump: Some(path.clone()),
                ..AskOptions::default()
            },
            true,
        );
        assert_eq!(params["arms"], "both");
        assert_eq!(params["judge.mode"], "model");
        assert_eq!(params["judge.same_model_as_answers"], true);
        for arm in ["plain", "grounded"] {
            assert_eq!(value(&m, &format!("ask.{arm}.judge_coverage")), 1.0);
            assert_eq!(value(&m, &format!("ask.{arm}.judge_correct_rate")), 1.0);
        }
        let dumped = std::fs::read_to_string(&path).unwrap();
        let verdicts = std::fs::read_to_string(dump::verdict_path(&path)).unwrap();
        assert_eq!(dumped.lines().count(), 16);
        assert_eq!(verdicts.lines().count(), 16);
        // Answers are dumped before any verdict exists.
        assert!(dumped.lines().all(|l| !l.contains("\"judge\"")));
        let mut ctx = RunContext::new(RunConfig::default());
        let again = rescore(&mut ctx, &gold::tests::sample(), &path).unwrap();
        assert_eq!(value(&again, "ask.grounded.judge_correct_rate"), 1.0);
    }
}
