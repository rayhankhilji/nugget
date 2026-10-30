//! `llama-server` command lines, derived from the role and the hardware.

use std::ffi::OsString;
use std::path::PathBuf;

use nugget_core::ipc::RuntimeRole;
use nugget_core::{Error, Result};
use nugget_models::{GIB, Hardware, RuntimeAccel};

/// How many model layers to offload to the GPU (`-ngl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuLayers {
    /// `0`: CPU only.
    None,
    /// `auto`: let llama-server fit as many layers as free VRAM allows.
    Auto,
    /// `all`: every layer (unified-memory GPUs).
    All,
    Count(u32),
}

impl GpuLayers {
    pub fn as_arg(self) -> String {
        match self {
            GpuLayers::None => "0".into(),
            GpuLayers::Auto => "auto".into(),
            GpuLayers::All => "all".into(),
            GpuLayers::Count(n) => n.to_string(),
        }
    }

    /// The default for a runtime build: CPU builds offload nothing, Metal (unified memory)
    /// everything, discrete-GPU builds (Vulkan, CUDA) as much as fits.
    pub fn for_accel(accel: RuntimeAccel) -> Self {
        match accel {
            RuntimeAccel::Cpu => GpuLayers::None,
            RuntimeAccel::Metal => GpuLayers::All,
            RuntimeAccel::Cuda | RuntimeAccel::Vulkan => GpuLayers::Auto,
        }
    }
}

/// Embedding pooling (`--pooling`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// Do not pass `--pooling`: llama-server uses the `pooling_type` stored in the GGUF, which is
    /// what the model was trained with.
    Model,
    None,
    Mean,
    Cls,
    Last,
    Rank,
}

impl Pooling {
    fn as_arg(self) -> Option<&'static str> {
        match self {
            Pooling::Model => None,
            Pooling::None => Some("none"),
            Pooling::Mean => Some("mean"),
            Pooling::Cls => Some("cls"),
            Pooling::Last => Some("last"),
            Pooling::Rank => Some("rank"),
        }
    }
}

/// Per-role overrides of the hardware-derived defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tuning {
    /// Context tokens per parallel slot.
    pub ctx_per_slot: Option<u32>,
    /// Parallel request slots (`--parallel`).
    pub parallel: Option<u32>,
    pub gpu_layers: Option<GpuLayers>,
    /// CPU threads (`--threads`); llama-server picks when unset.
    pub threads: Option<u32>,
    /// Embedding role only (the rerank role always pools by rank).
    pub pooling: Option<Pooling>,
    /// Extra arguments appended verbatim. Flags that would change the listen address, the API
    /// key or make llama-server fetch models from the network are refused.
    pub extra_args: Vec<String>,
}

/// Flags callers may not pass through [`Tuning::extra_args`]: they would expose the server beyond
/// loopback, replace or leak the per-process API key, or make llama-server download things.
const FORBIDDEN_FLAGS: &[&str] = &[
    "--host",
    "--port",
    "--reuse-port",
    "--api-key",
    "--api-key-file",
    "-m",
    "--model",
    "-mu",
    "--model-url",
    "-hf",
    "-hfr",
    "--hf-repo",
    "-hff",
    "--hf-file",
    "-hft",
    "--hf-token",
    "-dr",
    "--docker-repo",
    "--connect",
    "--ui-mcp-proxy",
    "--webui-mcp-proxy",
    "--ssl-key-file",
    "--ssl-cert-file",
    "--props",
    "--slot-save-path",
    "--log-file",
    "--log-prompts-dir",
];

/// Everything needed to start one `llama-server`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub role: RuntimeRole,
    /// The model file (a verified blob in the model store).
    pub model_path: PathBuf,
    /// Name the OpenAI-compatible API knows the model by (`--alias`), the model id.
    pub alias: String,
    /// Loopback port (host `127.0.0.1`), when `socket` is `None`.
    pub port: u16,
    /// Listen on this Unix socket (`--host <path>.sock`) instead of TCP. Unix only.
    pub socket: Option<PathBuf>,
    pub ctx_per_slot: u32,
    pub parallel: u32,
    pub gpu_layers: GpuLayers,
    /// `--batch-size` and `--ubatch-size` (embedding and rerank roles: a whole input must fit
    /// one ubatch).
    pub batch_size: Option<u32>,
    pub pooling: Pooling,
    pub threads: Option<u32>,
    pub extra_args: Vec<String>,
}

impl LaunchPlan {
    /// Derive a plan from role, hardware and build. `tuning` overrides individual values.
    pub fn new(
        role: RuntimeRole,
        model_path: PathBuf,
        alias: impl Into<String>,
        port: u16,
        accel: RuntimeAccel,
        hw: &Hardware,
        tuning: &Tuning,
    ) -> Result<Self> {
        for a in &tuning.extra_args {
            let flag = a.split('=').next().unwrap_or(a);
            if FORBIDDEN_FLAGS.contains(&flag) {
                return Err(Error::Invalid(format!(
                    "runtime extra argument `{flag}` is not allowed (it controls the listen \
                     address, the API key or network downloads)"
                )));
            }
        }
        let ram_gib = hw.total_ram / GIB;
        let cores = u32::try_from(hw.cpu_cores).unwrap_or(u32::MAX).max(1);
        let (ctx_default, parallel_default, batch) = match role {
            RuntimeRole::Chat => {
                let ctx = match ram_gib {
                    0..12 => 4096,
                    12..24 => 8192,
                    _ => 16384,
                };
                let parallel = if ram_gib >= 16 && cores >= 8 { 2 } else { 1 };
                (ctx, parallel, false)
            }
            RuntimeRole::Embedding => (2048, (cores / 2).clamp(1, 4), true),
            // A query and one passage per sequence (the reranker gets at most ~2000 characters
            // of each passage); like embeddings, a whole sequence must fit one ubatch.
            RuntimeRole::Rerank => (2048, (cores / 2).clamp(1, 4), true),
            // One Double Tap prompt (rules, voice, thread) plus its reply. A single slot, so the
            // prefix a read-ahead prefilled is the one the trigger finds in the cache.
            RuntimeRole::Draft => (8192, 1, false),
        };
        let ctx_per_slot = tuning.ctx_per_slot.unwrap_or(ctx_default).max(256);
        let parallel = tuning.parallel.unwrap_or(parallel_default).clamp(1, 64);
        Ok(Self {
            role,
            model_path,
            alias: alias.into(),
            port,
            socket: None,
            ctx_per_slot,
            parallel,
            gpu_layers: tuning
                .gpu_layers
                .unwrap_or_else(|| GpuLayers::for_accel(accel)),
            batch_size: batch.then_some(ctx_per_slot),
            pooling: match role {
                RuntimeRole::Embedding => tuning.pooling.unwrap_or(Pooling::Model),
                // `--reranking` implies rank pooling; a pooling override would break scoring.
                RuntimeRole::Rerank => Pooling::Rank,
                RuntimeRole::Chat | RuntimeRole::Draft => Pooling::Model,
            },
            threads: tuning.threads,
            extra_args: tuning.extra_args.clone(),
        })
    }

    /// Total context (`--ctx-size`): llama-server splits it across the parallel slots.
    pub fn ctx_size(&self) -> u32 {
        self.ctx_per_slot.saturating_mul(self.parallel)
    }

    /// The command-line arguments (without the program). Never contains the API key: it is
    /// passed through the `LLAMA_API_KEY` environment variable so it does not show up in process
    /// listings.
    pub fn args(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = Vec::with_capacity(32);
        match &self.socket {
            // llama-server listens on a Unix socket when the host ends in `.sock`.
            Some(path) => {
                a.push("--host".into());
                a.push(path.clone().into_os_string());
            }
            None => {
                a.push("--host".into());
                a.push("127.0.0.1".into());
                a.push("--port".into());
                a.push(self.port.to_string().into());
            }
        }
        let mut push = |k: &str, v: Option<String>| {
            a.push(k.into());
            if let Some(v) = v {
                a.push(v.into());
            }
        };
        push("--alias", Some(self.alias.clone()));
        push("--ctx-size", Some(self.ctx_size().to_string()));
        push("--parallel", Some(self.parallel.to_string()));
        push("--n-gpu-layers", Some(self.gpu_layers.as_arg()));
        if let Some(t) = self.threads {
            push("--threads", Some(t.to_string()));
        }
        match self.role {
            RuntimeRole::Embedding => {
                push("--embedding", None);
                if let Some(p) = self.pooling.as_arg() {
                    push("--pooling", Some(p.into()));
                }
            }
            // Serves `/v1/rerank`; sets embedding mode and rank pooling itself.
            RuntimeRole::Rerank => push("--reranking", None),
            RuntimeRole::Chat | RuntimeRole::Draft => {}
        }
        if let Some(b) = self.batch_size {
            push("--batch-size", Some(b.to_string()));
            push("--ubatch-size", Some(b.to_string()));
        }
        // No web UI, no downloads, no slot-inspection endpoint; metrics feed the idle detector.
        push("--no-webui", None);
        push("--offline", None);
        push("--no-slots", None);
        push("--metrics", None);
        for x in &self.extra_args {
            a.push(x.into());
        }
        a.push("--model".into());
        a.push(self.model_path.clone().into_os_string());
        a
    }
}
