//! `--local-models`: real local models through the local runtime, as users run them.
//!
//! The manifest bundled with the app (`nugget_models::default_manifest`, signature-verified)
//! names the models and the `llama-server` builds. [`LocalModels::start`] installs the chosen
//! models into a [`ModelStore`] (SHA-256 verified, cached under `--models-home` between runs),
//! then lets [`nugget_runtime::Runtime`] install the runtime build for this machine and start
//! one server per role, exactly as `nuggetd` does. Suites build their providers from the
//! configs it returns ([`LocalModels::chat`], [`LocalModels::embedder`]).
//!
//! - **Models.** By default the smallest chat model (any of the triage, light, synthesis or
//!   agentic tiers) and the smallest embedding model in the manifest; `--local-chat-model` and
//!   `--local-embed-model` pick others by manifest id.
//! - **Thinking.** Hybrid reasoning models (Qwen3) think before answering unless told not to;
//!   llama-server returns that as `reasoning_content`, which no Nugget feature shows, so it only
//!   adds latency. The runtime's chat config turns it off
//!   (`chat_template_kwargs.enable_thinking = false`), as the app runs it; [`Thinking::Off`] (the
//!   default) keeps that, [`Thinking::Model`] restores the model's default (for measuring what
//!   thinking costs).
//! - **Hardware** ([`HostInfo`]) is recorded in every report that uses a local model: CPU model,
//!   logical cores, total RAM, GPU kind, OS and architecture.
//!
//! The servers live on a runtime owned by [`LocalModels`]; dropping it stops them.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use nugget_core::capability::Tier;
use nugget_core::egress::{EgressSink, NullEgress};
use nugget_core::ipc::RuntimeRole;
use nugget_core::paths::Paths;
use nugget_infer::{Provider, ProviderConfig, build_provider_with};
use nugget_models::{GIB, Hardware, ModelEntry, ModelStore, default_manifest, detect_hardware};
use nugget_runtime::Runtime;
use serde_json::{Value, json};

use crate::runner::{BenchError, RunContext};

/// Chat tiers: a model serving any of them can draft, extract and drive the agent.
const CHAT_TIERS: [Tier; 4] = [Tier::Triage, Tier::Light, Tier::Synthesis, Tier::Agentic];

/// Whether a hybrid reasoning model may think before it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, clap::ValueEnum)]
pub enum Thinking {
    /// `enable_thinking = false` in the chat template.
    #[default]
    Off,
    /// The model's own default (Qwen3: thinks first).
    Model,
}

impl Thinking {
    pub fn as_str(self) -> &'static str {
        match self {
            Thinking::Off => "off",
            Thinking::Model => "model_default",
        }
    }
}

/// Local-model options (flattened into the CLI).
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct LocalOptions {
    /// `draft`, `doubletap`, `cua --cua-agent nugget`, `knowledge`: run against real local
    /// models: the signed manifest's models and `llama-server` build, installed and supervised
    /// by `nugget-runtime` as the app does.
    #[arg(long)]
    pub local_models: bool,
    /// Where `--local-models` installs models and the runtime (kept between runs; default
    /// `bench/.local-models`).
    #[arg(long, value_name = "DIR", env = "NUGGET_BENCH_MODELS_HOME")]
    pub models_home: Option<PathBuf>,
    /// Manifest id of the chat model (default: the smallest chat model in the manifest).
    #[arg(long, value_name = "ID", requires = "local_models")]
    pub local_chat_model: Option<String>,
    /// Manifest id of the embedding model (default: the smallest one in the manifest).
    #[arg(long, value_name = "ID", requires = "local_models")]
    pub local_embed_model: Option<String>,
    /// CPU threads per server (default: llama-server's choice).
    #[arg(long, value_name = "N", requires = "local_models")]
    pub local_threads: Option<u32>,
    /// Hybrid reasoning models: let them think first (`model`) or not (`off`, the default).
    #[arg(long, value_enum, default_value_t = Thinking::Off)]
    pub local_thinking: Thinking,
}

/// Default `--models-home`: `bench/.local-models` in the source tree (git-ignored).
pub const DEFAULT_MODELS_HOME: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/.local-models");

/// The machine a local run happened on.
#[derive(Debug, Clone, PartialEq)]
pub struct HostInfo {
    pub cpu_model: String,
    pub logical_cores: usize,
    pub total_ram_gib: f64,
    pub gpu: String,
    pub os: &'static str,
    pub arch: &'static str,
    /// Total RAM is at least the PLAN's "16 GB machine" (nominal, 10% slack as in
    /// `nugget_models::recommend`).
    pub meets_16gb: bool,
}

impl HostInfo {
    pub fn detect() -> Self {
        Self::from_hardware(&detect_hardware())
    }

    pub fn from_hardware(hw: &Hardware) -> Self {
        let total_ram_gib = hw.total_ram as f64 / GIB as f64;
        Self {
            cpu_model: cpu_model().unwrap_or_else(|| "unknown".into()),
            logical_cores: hw.cpu_cores,
            total_ram_gib,
            gpu: format!("{:?}", hw.gpu),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            meets_16gb: total_ram_gib * 10.0 / 9.0 >= 16.0,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "cpu_model": self.cpu_model,
            "logical_cores": self.logical_cores,
            "total_ram_gib": (self.total_ram_gib * 10.0).round() / 10.0,
            "gpu": self.gpu,
            "os": self.os,
            "arch": self.arch,
            "meets_16gb": self.meets_16gb,
        })
    }
}

/// CPU model string, best effort.
fn cpu_model() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        info.lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (!s.is_empty()).then_some(s)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        std::env::var("PROCESSOR_IDENTIFIER").ok()
    }
}

/// One model the runtime serves.
#[derive(Debug, Clone)]
pub struct Served {
    pub entry: ModelEntry,
    /// The config `Runtime::ensure` returned (key reference included).
    pub config: ProviderConfig,
    /// Seconds from `ensure` to ready (server start and model load).
    pub load_s: f64,
}

/// Running local servers. Dropping it stops them.
pub struct LocalModels {
    rt: tokio::runtime::Runtime,
    runtime: Runtime,
    pub chat: Option<Served>,
    pub embedding: Option<Served>,
    pub host: HostInfo,
    pub runtime_build: String,
    pub thinking: Thinking,
    /// Seconds spent installing (downloading and verifying) models; zero on a warm cache.
    pub install_s: f64,
}

impl std::fmt::Debug for LocalModels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalModels")
            .field("chat", &self.chat.as_ref().map(|s| &s.entry.id))
            .field("embedding", &self.embedding.as_ref().map(|s| &s.entry.id))
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// The smallest manifest model serving any of `tiers` (rerankers excluded).
pub fn smallest<'a>(models: &'a [ModelEntry], tiers: &[Tier]) -> Option<&'a ModelEntry> {
    models
        .iter()
        .filter(|m| !m.tiers.contains(&Tier::Rerank))
        .filter(|m| m.tiers.iter().any(|t| tiers.contains(t)))
        .min_by_key(|m| (m.size, m.id.clone()))
}

/// The model `id` from the manifest, or the smallest one serving `tiers`.
pub fn pick<'a>(
    models: &'a [ModelEntry],
    id: Option<&str>,
    tiers: &[Tier],
) -> Result<&'a ModelEntry, BenchError> {
    match id {
        Some(id) => models
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| BenchError::Failed(format!("model `{id}` is not in the manifest"))),
        None => smallest(models, tiers)
            .ok_or_else(|| BenchError::Failed(format!("the manifest has no model for {tiers:?}"))),
    }
}

impl LocalModels {
    /// Install and start the requested roles (`Chat` and/or `Embedding`).
    pub fn start(opts: &LocalOptions, roles: &[RuntimeRole]) -> Result<Self, BenchError> {
        let home = opts
            .models_home
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_MODELS_HOME));
        std::fs::create_dir_all(&home)?;
        let paths = Paths::rooted(home);
        let manifest = default_manifest()?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let egress: Arc<dyn EgressSink> = Arc::new(NullEgress);
        let store = Arc::new(ModelStore::open(&paths, egress.clone())?);
        let mut runtime_opts = nugget_runtime::RuntimeOptions::default();
        runtime_opts.chat.threads = opts.local_threads;
        runtime_opts.embedding.threads = opts.local_threads;
        let runtime = Runtime::new(&paths, store.clone(), egress)?.with_options(runtime_opts);
        let host = HostInfo::from_hardware(runtime.hardware());
        let runtime_build = runtime
            .runtime_candidates()
            .first()
            .map(|r| format!("{} ({})", r.id, r.version))
            .unwrap_or_else(|| "none for this machine".into());

        let mut wanted = Vec::new();
        for role in roles {
            let entry = match role {
                RuntimeRole::Chat => pick(
                    &manifest.models,
                    opts.local_chat_model.as_deref(),
                    &CHAT_TIERS,
                )?,
                RuntimeRole::Embedding => pick(
                    &manifest.models,
                    opts.local_embed_model.as_deref(),
                    &[Tier::Embedding],
                )?,
                other => {
                    return Err(BenchError::Failed(format!(
                        "local models: role {other:?} is not used by the bench"
                    )));
                }
            };
            wanted.push((*role, entry.clone()));
        }

        let started = Instant::now();
        let mut served = Vec::new();
        rt.block_on(async {
            for (_, entry) in &wanted {
                if store.slot(&entry.id)?.is_none() || store.needs_update(entry)? {
                    eprintln!(
                        "local models: installing {} ({} MB)",
                        entry.id,
                        entry.size / 1_000_000
                    );
                    store.install(entry, |_| {}).await?;
                }
            }
            Ok::<_, nugget_core::Error>(())
        })?;
        let install_s = started.elapsed().as_secs_f64();
        for (role, entry) in wanted {
            let t = Instant::now();
            let config = rt.block_on(runtime.ensure(role, &entry))?;
            eprintln!(
                "local models: {} ready as {role:?} in {:.1} s",
                entry.id,
                t.elapsed().as_secs_f64()
            );
            served.push((
                role,
                Served {
                    entry,
                    config,
                    load_s: t.elapsed().as_secs_f64(),
                },
            ));
        }
        let take = |r: RuntimeRole| {
            served
                .iter()
                .find(|(role, _)| *role == r)
                .map(|(_, s)| s.clone())
        };
        Ok(Self {
            chat: take(RuntimeRole::Chat),
            embedding: take(RuntimeRole::Embedding),
            rt,
            runtime,
            host,
            runtime_build,
            thinking: opts.local_thinking,
            install_s,
        })
    }

    /// A provider for the chat server, thinking as configured.
    pub fn chat(&self) -> Result<Arc<dyn Provider>, BenchError> {
        self.chat_with(self.thinking)
    }

    /// A provider for the chat server with an explicit thinking setting.
    pub fn chat_with(&self, thinking: Thinking) -> Result<Arc<dyn Provider>, BenchError> {
        let served = self
            .chat
            .as_ref()
            .ok_or_else(|| BenchError::Failed("no local chat model was started".into()))?;
        let cfg = chat_config(&served.config, thinking);
        Ok(build_provider_with(
            &cfg,
            Arc::new(NullEgress),
            &self.runtime.secrets(),
        )?)
    }

    /// The chat config with the thinking setting applied (key reference included). For
    /// suites that build their own providers.
    pub fn chat_config(&self, thinking: Thinking) -> Option<ProviderConfig> {
        self.chat.as_ref().map(|s| chat_config(&s.config, thinking))
    }

    /// The resolver for the key references in the configs.
    pub fn secrets(&self) -> nugget_runtime::RuntimeSecrets {
        self.runtime.secrets()
    }

    /// A provider for the embedding server.
    pub fn embedder(&self) -> Result<Arc<dyn Provider>, BenchError> {
        let served = self
            .embedding
            .as_ref()
            .ok_or_else(|| BenchError::Failed("no local embedding model was started".into()))?;
        Ok(build_provider_with(
            &served.config,
            Arc::new(NullEgress),
            &self.runtime.secrets(),
        )?)
    }

    /// Short label for reports: `qwen3-4b-q4km (Qwen3-4B-Q4_K_M.gguf) via llama-server ...`.
    pub fn chat_label(&self) -> String {
        match &self.chat {
            Some(s) => format!(
                "{} ({}, local llama-server {}, thinking {})",
                s.entry.id,
                s.entry.file_name,
                self.runtime_build,
                self.thinking.as_str()
            ),
            None => "none".into(),
        }
    }

    /// Record model, quantisation, runtime and hardware as report parameters (`local.*`).
    pub fn record(&self, ctx: &mut RunContext) {
        ctx.param("local.runtime", self.runtime_build.clone());
        ctx.param("local.hardware", self.host.to_json());
        ctx.param("local.thinking", self.thinking.as_str());
        ctx.param("local.install_s", round2(self.install_s));
        for (key, s) in [
            ("local.chat", &self.chat),
            ("local.embedding", &self.embedding),
        ] {
            if let Some(s) = s {
                ctx.param(
                    key,
                    json!({
                        "id": s.entry.id,
                        "file": s.entry.file_name,
                        "quantisation": quantisation(&s.entry.file_name),
                        "revision": s.entry.revision,
                        "sha256": s.entry.sha256,
                        "size_bytes": s.entry.size,
                        "load_s": round2(s.load_s),
                    }),
                );
            }
        }
    }

    /// Block on `f` on the runtime that owns the servers.
    pub fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.rt.block_on(f)
    }
}

impl Drop for LocalModels {
    fn drop(&mut self) {
        let runtime = self.runtime.clone();
        self.rt.block_on(runtime.shutdown());
    }
}

fn chat_config(cfg: &ProviderConfig, thinking: Thinking) -> ProviderConfig {
    let mut cfg = cfg.clone();
    let mut body = cfg.extra_body.take().unwrap_or_else(|| json!({}));
    if let Some(obj) = body.as_object_mut() {
        let kwargs = obj
            .entry("chat_template_kwargs")
            .or_insert_with(|| json!({}));
        if let Some(k) = kwargs.as_object_mut() {
            match thinking {
                Thinking::Off => {
                    k.insert("enable_thinking".into(), json!(false));
                }
                // The runtime turns thinking off; `Model` restores the template's default.
                Thinking::Model => {
                    k.remove("enable_thinking");
                }
            }
            if k.is_empty() {
                obj.remove("chat_template_kwargs");
            }
        }
    }
    cfg.extra_body = body
        .as_object()
        .is_some_and(|o| !o.is_empty())
        .then_some(body);
    cfg
}

/// The quantisation in a GGUF file name (`Qwen3-4B-Q4_K_M.gguf` → `Q4_K_M`).
pub fn quantisation(file_name: &str) -> String {
    let stem = file_name.strip_suffix(".gguf").unwrap_or(file_name);
    stem.rsplit(['-', '.'])
        .find(|p| {
            let u = p.to_ascii_uppercase();
            (u.starts_with('Q') || u.starts_with("IQ") || u == "F16" || u == "BF16" || u == "F32")
                && u.chars().any(|c| c.is_ascii_digit())
        })
        .map_or_else(|| "unknown".into(), str::to_owned)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, size: u64, tiers: &[Tier]) -> ModelEntry {
        let mut m: ModelEntry = serde_json::from_value(json!({
            "id": id, "family": "x", "file_name": format!("{id}.gguf"),
            "url": "https://example.invalid/0000000000000000000000000000000000000000/x.gguf",
            "revision": "0000000000000000000000000000000000000000",
            "size": size, "sha256": "00", "min_ram_gb": 4, "tiers": [],
            "license": "MIT", "min_app_version": "0.0.1"
        }))
        .unwrap();
        m.tiers = tiers.to_vec();
        m
    }

    #[test]
    fn picks_the_smallest_model_per_role() {
        let models = vec![
            entry("big-chat", 5, &[Tier::Synthesis, Tier::Agentic]),
            entry("small-chat", 2, &[Tier::Triage, Tier::Synthesis]),
            entry("embed", 1, &[Tier::Embedding]),
            entry("rerank", 0, &[Tier::Rerank]),
        ];
        assert_eq!(pick(&models, None, &CHAT_TIERS).unwrap().id, "small-chat");
        assert_eq!(pick(&models, None, &[Tier::Embedding]).unwrap().id, "embed");
        assert_eq!(
            pick(&models, Some("big-chat"), &CHAT_TIERS).unwrap().id,
            "big-chat"
        );
        assert!(pick(&models, Some("nope"), &CHAT_TIERS).is_err());
    }

    #[test]
    fn the_bundled_manifest_has_a_chat_and_an_embedding_model() {
        let m = default_manifest().unwrap();
        let chat = pick(&m.models, None, &CHAT_TIERS).unwrap();
        let embed = pick(&m.models, None, &[Tier::Embedding]).unwrap();
        assert!(!chat.tiers.contains(&Tier::Embedding));
        assert!(embed.tiers.contains(&Tier::Embedding));
    }

    #[test]
    fn thinking_off_merges_the_template_flag() {
        let cfg: ProviderConfig = serde_json::from_value(json!({
            "id": "local-chat", "kind": "openai_compatible",
            "base_url": "http://127.0.0.1:1/v1", "model": "m",
            "extra_body": {"cache_prompt": true}
        }))
        .unwrap();
        let off = chat_config(&cfg, Thinking::Off);
        let body = off.extra_body.clone().unwrap();
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(body["cache_prompt"], true);
        assert_eq!(chat_config(&cfg, Thinking::Model), cfg);
        // The runtime's own config turns thinking off; `Model` takes that back out.
        assert_eq!(chat_config(&off, Thinking::Model), cfg);
    }

    #[test]
    fn quantisation_from_file_names() {
        assert_eq!(quantisation("Qwen3-4B-Q4_K_M.gguf"), "Q4_K_M");
        assert_eq!(quantisation("nomic-embed-text-v1.5.Q8_0.gguf"), "Q8_0");
        assert_eq!(quantisation("model.gguf"), "unknown");
    }

    #[test]
    fn host_info_flags_the_16gb_assumption() {
        let hw = |gib: u64| Hardware {
            total_ram: gib * GIB,
            available_ram: 0,
            cpu_cores: 4,
            gpu: nugget_models::GpuKind::Unknown,
        };
        assert!(HostInfo::from_hardware(&hw(15)).meets_16gb);
        assert!(!HostInfo::from_hardware(&hw(8)).meets_16gb);
    }
}
