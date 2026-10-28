//! `bench/draft`: Double Tap reply drafting (milestone M3), Nugget versus a Sentient-style
//! baseline. See `bench/README.md` for the methodology.
//!
//! Both arms write the same prompt format ([`prompt`]) for the same model; only the content
//! differs ([`arms`]). The default model is the deterministic [`fake`] one, so CI numbers are
//! reproducible; `--provider-url` runs a real model through `nugget-infer`.

pub mod arms;
pub mod fake;
pub mod fixtures;
pub mod prompt;
pub mod score;
pub mod style;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use nugget_core::capability::Locality;
use nugget_core::egress::NullEgress;
use nugget_infer::{ChatEvent, ChatRequest, Message, Provider, ProviderConfig, build_provider};
use serde_json::json;

use crate::report::{Direction, Metric};
use crate::runner::{BenchError, Benchmark, RunContext};
use arms::{BaselineArm, NuggetArm, World};
use fixtures::Corpus;
use prompt::Prompt;
use score::{ArmSummary, ScoreInput, TaskScore};

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
}

enum Model {
    Fake(fake::Latency),
    Real(Arc<dyn Provider>),
}

struct Generation {
    text: String,
    ttft: Duration,
    total: Duration,
}

impl Model {
    fn build(o: &DraftOptions) -> Result<(Self, Locality, String), BenchError> {
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
            return Ok((Self::Fake(latency), locality, "fake-deterministic".into()));
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
        let locality = provider.meta().locality;
        Ok((
            Self::Real(provider),
            locality,
            format!("{kind} {url} model {model}"),
        ))
    }

    /// Generate a reply. Times are measured from `started` (before the prompt was built).
    async fn generate(&self, p: &Prompt, started: Instant) -> Result<Generation, BenchError> {
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
            Self::Real(provider) => {
                let req = ChatRequest::new(vec![Message::user_text(p.user.clone())])
                    .with_system(p.system.clone())
                    .with_max_tokens(400)
                    .with_feature("draft");
                let mut stream = provider.chat(req).await?;
                let mut text = String::new();
                let mut ttft = None;
                while let Some(ev) = stream.next().await {
                    match ev? {
                        ChatEvent::TextDelta(d) => {
                            if ttft.is_none() && !d.is_empty() {
                                ttft = Some(started.elapsed());
                            }
                            text.push_str(&d);
                        }
                        ChatEvent::Done { .. } => break,
                        _ => {}
                    }
                }
                let total = started.elapsed();
                Ok(Generation {
                    text: text.trim().to_owned(),
                    ttft: ttft.unwrap_or(total),
                    total,
                })
            }
        }
    }
}

/// Everything a run produced, before it becomes metrics.
pub struct DraftRun {
    pub nugget: Vec<TaskScore>,
    pub baseline: Vec<TaskScore>,
    pub nugget_drafts: Vec<String>,
    pub baseline_drafts: Vec<String>,
    pub field_kind_correct: usize,
    pub recipient_profiles: usize,
    pub excluded_sensitive: u32,
    pub neutralised: usize,
}

/// Run both arms over `tasks` (indices into the corpus) and score them.
async fn run_arms(
    world: &World,
    tasks: &[usize],
    model: &Model,
    exclude_sensitive: bool,
    seed: u64,
) -> Result<DraftRun, BenchError> {
    let nugget_arm = NuggetArm::new(world);
    let baseline_arm = BaselineArm::new(world, seed);
    let mut run = DraftRun {
        nugget: Vec::new(),
        baseline: Vec::new(),
        nugget_drafts: Vec::new(),
        baseline_drafts: Vec::new(),
        field_kind_correct: 0,
        recipient_profiles: 0,
        excluded_sensitive: 0,
        neutralised: 0,
    };
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
                baseline_arm.prompt(task)?
            };
            let g = model.generate(&prompt, started).await?;
            let s = score::score(&ScoreInput {
                task,
                draft: &g.text,
                prompt_text: &prompt.text(),
                prompt_tokens: prompt.tokens(),
                channel_profile: &channel_profile,
                secrets: &world.corpus.secrets,
                ttft: g.ttft,
                total: g.total,
            });
            if arm == 0 {
                run.nugget.push(s);
                run.nugget_drafts.push(g.text);
            } else {
                run.baseline.push(s);
                run.baseline_drafts.push(g.text);
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
        let corpus = Corpus::load().map_err(BenchError::Failed)?;
        let (model, locality, model_desc) = Model::build(&opts)?;
        let real = matches!(model, Model::Real(_));
        let cloud = locality != Locality::OnDevice;
        let all = corpus.tasks.len();
        // The fake model is cheap: always every task. A real one is limited by scale.
        let limit = if real {
            ctx.scale().pick(6, 20, all, all).min(all)
        } else {
            all
        };
        let tasks: Vec<usize> = spread(all, limit);

        ctx.param("model", model_desc);
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
        if !real {
            ctx.param("fake.ttft_ms", opts.fake_ttft_ms);
            ctx.param("fake.prefill_ms_per_1k", opts.fake_prefill_ms_per_1k);
            ctx.param("fake.token_ms", opts.fake_token_ms);
        }

        let world = World::build(corpus)?;
        let vault_bytes = BaselineArm::new(&world, ctx.seed()).vault_bytes();
        ctx.param("baseline.vault_bytes", vault_bytes);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let run = rt.block_on(run_arms(&world, &tasks, &model, cloud, ctx.seed()))?;
        ctx.param("nugget.excluded_sensitive", run.excluded_sensitive);
        ctx.param("nugget.neutralised_sentences", run.neutralised);
        ctx.param("nugget.recipient_profiles", run.recipient_profiles);
        if let Some(path) = &opts.draft_dump {
            dump(path, &world, &tasks, &run)?;
        }
        Ok(metrics(&run, real, locality))
    }
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
}
