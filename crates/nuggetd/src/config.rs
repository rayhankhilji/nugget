//! `config.toml`: model providers (the `nugget-infer` [`ProviderConfig`] format) and local model
//! settings. A commented, local-first default is written on first run. Secrets never live here:
//! providers reference API keys as `env:VAR` or `keychain:NAME`.

use std::collections::HashSet;
use std::path::Path;

use nugget_core::capability::Tier;
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_infer::ProviderConfig;
use nugget_ingest::IngestConfig;
use nugget_knowledge::KnowledgeConfig;
use nugget_retrieve::RetrieveConfig;
use nugget_vault::VaultConfig;
use serde::{Deserialize, Serialize};

/// Written to `Paths::config_file()` when no config exists.
pub const DEFAULT_CONFIG: &str = r#"# Nugget daemon (nuggetd) configuration.
#
# Nugget is local-first. Out of the box every tier below is served by a model running on this
# machine, behind an OpenAI-compatible server (Ollama on its default port), so nothing leaves the
# device until you add a cloud provider.
#
# Never put secrets in this file. Providers reference API keys as "env:VAR" (an environment
# variable) or "keychain:NAME" (an OS keychain entry).
#
# Tiers: triage | light | synthesis | agentic | embedding. When several providers serve a tier
# they are tried in file order, filtered by the capabilities their probe proved. Unprobed
# providers only serve plain chat; probe one from the app or the providers.probe IPC method.
#
# Provider fields: id, kind (anthropic | openai_responses | openai_compatible), base_url, model,
# api_key, tiers. Optional: locality, context_tokens, max_output_tokens, connect_timeout_secs,
# idle_timeout_secs, max_retries, retry_base_ms, headers, extra_body.
# Restart nuggetd after editing this file.

[models]
# Parallel connections per local model download.
download_parts = 4

# The local model chosen for each tier (ids from the app's Models page). The on-device runtime
# below falls back to `triage` (chat) and `embedding` when [runtime] names no model.
[models.selected]
# triage = "qwen3-4b-q4km"
# embedding = "nomic-embed-text-v1.5-q8"

# --- On-device runtime (llama.cpp llama-server, managed by nuggetd) ------------------------------
# When a selected model is installed (Models page), nuggetd runs it on 127.0.0.1 and routes to it
# as `local-chat` (triage, light, synthesis) and `local-embedding` (embedding). Providers you
# configure below that run on this device or your network are tried first; cloud providers come
# after the runtime and are never used for triage or (by default) embeddings.
[runtime]
enabled = true
# Manifest model ids. Unset: [models.selected], then the recommendation for this machine.
# chat_model = "qwen3-4b-q4km"
# embedding_model = "nomic-embed-text-v1.5-q8"
# A reranker that reorders search results (served as `local-rerank`; never chosen automatically).
# rerank_model = "bge-reranker-v2-m3-q4km"
# Or one you run yourself (`llama-server --reranking ...`), which takes precedence:
# [retrieve.reranker]
# base_url = "http://127.0.0.1:8013"
# Stop a server after this many idle minutes (0 = keep running); it restarts on the next request.
idle_stop_minutes = 30

# --- Knowledge graph ---------------------------------------------------------------------------
# People, organisations, projects and facts extracted from your indexed items, every fact cited.
# Extraction runs after indexing, periodically and at start. Deterministic extractors always run;
# the LLM extractor only uses models on this device or your network unless you list sources in
# `llm.cloud_sources` (sensitive items never go to the cloud).
[knowledge]
enabled = true
# Who you are, so mail and calendar facts hang off your own entity.
# [knowledge.me]
# name = "Your Name"
# emails = ["you@example.com"]

# --- Markdown vault --------------------------------------------------------------------------------
# One Markdown page per entity, with footnote citations. Your edits are merged back: changed or
# added fact lines become facts you asserted. Default location: ~/Nugget Vault.
[vault]
enabled = true
# path = "~/Nugget Vault"

# --- MCP (Model Context Protocol) -----------------------------------------------------------------
# Lets AI apps (Claude Desktop, Claude Code, Cursor) read your knowledge with a per-app token,
# scoped and audited. Local only: stdio through `nuggetd mcp --client <id>` and, when `http` is
# on, streamable HTTP on 127.0.0.1. Set apps up with `nuggetd mcp install --client <app>`.
[mcp]
enabled = true
http = true
port = 7319

# --- On-device: Ollama ---------------------------------------------------------------------------
# Install Ollama (https://ollama.com), then pull the model, e.g. `ollama pull qwen3:4b`.
[[providers]]
id = "local"
kind = "openai_compatible"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen3:4b"
tiers = ["triage", "light", "synthesis"]

# --- Cloud: Anthropic (uncomment to use; requests are recorded in the egress ledger) -------------
# Set the key in your environment, or store it in the OS keychain and use "keychain:anthropic".
# [[providers]]
# id = "anthropic"
# kind = "anthropic"
# model = "claude-opus-5-5"
# api_key = "env:ANTHROPIC_API_KEY"
# tiers = ["synthesis", "agentic"]

# --- Cloud: OpenAI (uncomment to use; requests are recorded in the egress ledger) ----------------
# [[providers]]
# id = "openai"
# kind = "openai_responses"
# model = "gpt-5"
# api_key = "env:OPENAI_API_KEY"
# tiers = ["synthesis", "agentic"]
"#;

/// The whole `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub models: ModelsConfig,
    /// `[runtime]`: the on-device llama-server runtime.
    #[serde(default)]
    pub runtime: RuntimeConfig,
    /// `[ingest]`: indexing tunables (`nugget_ingest::IngestConfig`; every field optional).
    #[serde(default)]
    pub ingest: IngestConfig,
    /// `[retrieve]`: search tunables (`nugget_retrieve::RetrieveConfig`; every field optional).
    #[serde(default)]
    pub retrieve: RetrieveConfig,
    /// `[knowledge]`: the extraction pipeline.
    #[serde(default)]
    pub knowledge: KnowledgeSection,
    /// `[vault]`: the Markdown vault.
    #[serde(default)]
    pub vault: VaultSection,
    /// `[mcp]`: the MCP server.
    #[serde(default)]
    pub mcp: McpSection,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Sections owned by daemon features (`crate::features`), each parsed by its feature with
    /// `features::section`. Kept raw here so a feature's settings live next to its code.
    /// `[draft]`: Double Tap drafting and the voice profile.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub draft: toml::Table,
    /// `[agent]`: the agent runtime and computer use.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub agent: toml::Table,
    /// `[policy]`: the safety policy for actions.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub policy: toml::Table,
    /// `[browser]`: the browser extension bridge.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub browser: toml::Table,
    /// `[proactive]`: proactive cards and the morning brief.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub proactive: toml::Table,
    /// `[plugins]`: WebAssembly plugins.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub plugins: toml::Table,
}

/// `[knowledge]`: scheduling, plus every `nugget_knowledge::KnowledgeConfig` field (`me`, `llm`,
/// `resolution`, ...) passed through as is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgeSection {
    /// Run extraction at all (default true).
    pub enabled: bool,
    /// Run again this often even without new items (retries of LLM extraction); 0 = never.
    pub interval_minutes: u64,
    /// Wait this long after the last indexed change before a run.
    pub debounce_ms: u64,
    /// Items processed per run at most; a run that leaves items is followed by another one.
    pub max_items_per_run: usize,
    /// LLM extraction requests per run at most (none while the power policy is constrained).
    pub max_llm_requests_per_run: usize,
    /// Wall-clock limit of one run, in seconds (0 = none).
    pub max_run_seconds: u64,
    /// The pipeline's own configuration.
    #[serde(flatten)]
    pub pipeline: KnowledgeConfig,
}

impl Default for KnowledgeSection {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_minutes: 10,
            debounce_ms: 1500,
            max_items_per_run: 500,
            max_llm_requests_per_run: 50,
            max_run_seconds: 120,
            pipeline: KnowledgeConfig::default(),
        }
    }
}

/// `[vault]`: where and how the graph is rendered as Markdown, plus every
/// `nugget_vault::VaultConfig` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultSection {
    /// Render and watch the vault (default true).
    pub enabled: bool,
    /// Vault directory; a leading `~` is the home directory. Unset: `~/Nugget Vault` for a normal
    /// install, `<data>/vault` when Nugget runs with `--home`/`NUGGET_HOME`.
    pub path: Option<String>,
    #[serde(flatten)]
    pub render: VaultConfig,
}

impl Default for VaultSection {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            render: VaultConfig::default(),
        }
    }
}

/// Default loopback port of the MCP HTTP listener.
pub const DEFAULT_MCP_PORT: u16 = 7319;

/// `[mcp]`: the MCP server (stdio bridge and loopback HTTP).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct McpSection {
    /// Serve MCP at all (clients, the stdio bridge and HTTP). Default true.
    pub enabled: bool,
    /// Listen for streamable HTTP on 127.0.0.1 (default true).
    pub http: bool,
    /// HTTP port on 127.0.0.1 (default 7319; 0 picks a free port).
    pub port: u16,
    /// Exact `Origin` values allowed on HTTP requests (e.g. the MCP Inspector's). Empty: any
    /// request with an `Origin` header (a browser page) is refused.
    pub allowed_origins: Vec<String>,
}

impl Default for McpSection {
    fn default() -> Self {
        Self {
            enabled: true,
            http: true,
            port: DEFAULT_MCP_PORT,
            allowed_origins: Vec::new(),
        }
    }
}

/// `[runtime]`: local inference through `nugget-runtime`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RuntimeConfig {
    /// Run selected, installed models locally (default true).
    pub enabled: bool,
    /// Manifest id of the chat model (served as `local-chat` for triage, light and synthesis).
    /// Unset: `models.selected.triage`, then the recommendation for the Triage tier (then
    /// Synthesis).
    pub chat_model: Option<String>,
    /// Manifest id of the embedding model (served as `local-embedding`). Unset:
    /// `models.selected.embedding`, then the recommendation for the Embedding tier.
    pub embedding_model: Option<String>,
    /// Manifest id of a cross-encoder reranker (served as `local-rerank`, used by search only,
    /// never routed for a tier). Unset (default): no local reranker; there is no automatic
    /// recommendation. `[retrieve.reranker]` takes precedence.
    pub rerank_model: Option<String>,
    /// Stop a server after this many minutes without requests; 0 keeps it running. A stopped
    /// server starts again on the next request.
    pub idle_stop_minutes: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            chat_model: None,
            embedding_model: None,
            rerank_model: None,
            idle_stop_minutes: 30,
        }
    }
}

/// `[models]`: local model settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelsConfig {
    /// Parallel connections per model download (1–16).
    pub download_parts: usize,
    /// `[models.selected]`: the local model chosen per tier.
    pub selected: TierSelection,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            download_parts: 4,
            selected: TierSelection::default(),
        }
    }
}

/// A model id per tier, as listed by `models.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TierSelection {
    pub triage: Option<String>,
    pub light: Option<String>,
    pub synthesis: Option<String>,
    pub agentic: Option<String>,
    pub embedding: Option<String>,
}

impl TierSelection {
    /// The selected model per tier, for tiers that have one.
    pub fn iter(&self) -> impl Iterator<Item = (Tier, &str)> {
        [
            (Tier::Triage, &self.triage),
            (Tier::Light, &self.light),
            (Tier::Synthesis, &self.synthesis),
            (Tier::Agentic, &self.agentic),
            (Tier::Embedding, &self.embedding),
        ]
        .into_iter()
        .filter_map(|(t, id)| id.as_deref().map(|id| (t, id)))
    }
}

impl Config {
    /// Parse and validate a config file's contents. Never touches the network or secrets.
    pub fn parse(s: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(s).map_err(|e| Error::Invalid(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// The built-in default (the parsed [`DEFAULT_CONFIG`]).
    pub fn default_local() -> Self {
        Self::parse(DEFAULT_CONFIG).expect("the built-in default config is valid")
    }

    pub fn validate(&self) -> Result<()> {
        let mut seen = HashSet::new();
        for p in &self.providers {
            p.validate()?;
            if !seen.insert(p.id.clone()) {
                return Err(Error::Invalid(format!("duplicate provider id `{}`", p.id)));
            }
        }
        if !(1..=16).contains(&self.models.download_parts) {
            return Err(Error::Invalid(format!(
                "models.download_parts must be between 1 and 16 (got {})",
                self.models.download_parts
            )));
        }
        for (tier, id) in self.models.selected.iter() {
            if !is_model_id(id) {
                return Err(Error::Invalid(format!(
                    "models.selected.{}: `{id}` is not a model id",
                    tier_name(tier)
                )));
            }
        }
        for (field, id) in [
            ("chat_model", &self.runtime.chat_model),
            ("embedding_model", &self.runtime.embedding_model),
            ("rerank_model", &self.runtime.rerank_model),
        ] {
            if let Some(id) = id
                && !is_model_id(id)
            {
                return Err(Error::Invalid(format!(
                    "runtime.{field}: `{id}` is not a model id"
                )));
            }
        }
        if let Some(r) = &self.retrieve.reranker {
            if r.base_url.trim().is_empty() {
                return Err(Error::Invalid(
                    "retrieve.reranker.base_url is required".into(),
                ));
            }
            // Same checks as at start-up (URL, local only unless allow_cloud), reported early.
            nugget_retrieve::HttpReranker::new(
                r,
                None,
                std::sync::Arc::new(nugget_core::egress::NullEgress),
            )
            .map_err(|e| Error::Invalid(format!("retrieve.reranker: {e}")))?;
        }
        if self.runtime.idle_stop_minutes > 7 * 24 * 60 {
            return Err(Error::Invalid(
                "runtime.idle_stop_minutes must be at most 10080 (one week)".into(),
            ));
        }
        let k = &self.knowledge;
        if k.max_items_per_run == 0 {
            return Err(Error::Invalid(
                "knowledge.max_items_per_run must be at least 1".into(),
            ));
        }
        if k.debounce_ms > 10 * 60 * 1000 || k.interval_minutes > 7 * 24 * 60 {
            return Err(Error::Invalid(
                "knowledge.debounce_ms must be at most 600000 and knowledge.interval_minutes at most 10080"
                    .into(),
            ));
        }
        if let Some(p) = &self.vault.path
            && p.trim().is_empty()
        {
            return Err(Error::Invalid("vault.path must not be empty".into()));
        }
        for o in &self.mcp.allowed_origins {
            if !(o.starts_with("http://") || o.starts_with("https://")) || o.ends_with('/') {
                return Err(Error::Invalid(format!(
                    "mcp.allowed_origins: `{o}` is not an origin like http://localhost:6274"
                )));
            }
        }
        Ok(())
    }

    /// Read `path`, or write [`DEFAULT_CONFIG`] there first if it does not exist. Returns the
    /// config and whether it was just created. Errors name the file.
    pub fn load_or_create_at(path: &Path) -> Result<(Self, bool)> {
        let created = match std::fs::read_to_string(path) {
            Ok(_) => false,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                write_default(path)?;
                true
            }
            Err(e) => {
                return Err(Error::Invalid(format!(
                    "cannot read {}: {e}",
                    path.display()
                )));
            }
        };
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", path.display())))?;
        let cfg = Self::parse(&text).map_err(|e| {
            Error::Invalid(format!("invalid config {}: {}", path.display(), msg(e)))
        })?;
        Ok((cfg, created))
    }

    /// [`Config::load_or_create_at`] on `Paths::config_file()`.
    pub fn load_or_create(paths: &Paths) -> Result<(Self, bool)> {
        Self::load_or_create_at(&paths.config_file())
    }
}

fn is_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// The inner message of an `Invalid` error (avoids "invalid input: invalid config: ...").
fn msg(e: Error) -> String {
    match e {
        Error::Invalid(m) => m,
        other => other.to_string(),
    }
}

pub(crate) fn tier_name(t: Tier) -> &'static str {
    match t {
        Tier::Triage => "triage",
        Tier::Light => "light",
        Tier::Synthesis => "synthesis",
        Tier::Agentic => "agentic",
        Tier::Embedding => "embedding",
    }
}

/// Create the config file without clobbering one that appeared concurrently.
fn write_default(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        crate::fsutil::create_private_dir(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(DEFAULT_CONFIG.as_bytes())?;
            f.sync_all()?;
            tracing::info!(path = %path.display(), "wrote default config");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::capability::Locality;
    use nugget_infer::ProviderKind;

    #[test]
    fn default_config_is_local_first() {
        let cfg = Config::default_local();
        assert_eq!(cfg.models, ModelsConfig::default());
        assert_eq!(cfg.runtime, RuntimeConfig::default());
        assert!(cfg.runtime.enabled);
        assert_eq!(cfg.providers.len(), 1);
        let p = &cfg.providers[0];
        assert_eq!(p.kind, ProviderKind::OpenaiCompatible);
        assert_eq!(p.base_url.as_deref(), Some("http://127.0.0.1:11434/v1"));
        assert_eq!(p.effective_locality().unwrap(), Locality::OnDevice);
        assert_eq!(p.tiers, vec![Tier::Triage, Tier::Light, Tier::Synthesis]);
        assert!(p.api_key.is_none());
    }

    #[test]
    fn default_config_has_no_secrets() {
        for line in DEFAULT_CONFIG.lines() {
            let l = line.trim_start_matches('#').trim();
            if let Some(v) = l.strip_prefix("api_key = ") {
                let v = v.trim_matches('"');
                assert!(
                    v.starts_with("env:") || v.starts_with("keychain:"),
                    "{line}"
                );
            }
        }
        assert!(!DEFAULT_CONFIG.contains("sk-"));
    }

    /// The commented-out cloud examples are valid once uncommented.
    #[test]
    fn commented_examples_parse_when_uncommented() {
        let start = DEFAULT_CONFIG.find("# [[providers]]").unwrap();
        let (head, tail) = DEFAULT_CONFIG.split_at(start);
        let mut text = head.to_string();
        for line in tail.lines() {
            let is_setting = line
                .strip_prefix("# ")
                .is_some_and(|l| l == "[[providers]]" || l.split_once(" = ").is_some());
            text.push_str(if is_setting { &line[2..] } else { line });
            text.push('\n');
        }
        let cfg = Config::parse(&text).unwrap();
        let kinds: Vec<_> = cfg.providers.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ProviderKind::OpenaiCompatible,
                ProviderKind::Anthropic,
                ProviderKind::OpenaiResponses
            ]
        );
        for p in &cfg.providers[1..] {
            assert_eq!(p.effective_locality().unwrap(), Locality::Cloud);
            assert!(p.api_key.is_some());
        }
    }

    #[test]
    fn empty_file_gives_defaults_without_providers() {
        let cfg = Config::parse("").unwrap();
        assert!(cfg.providers.is_empty());
        assert_eq!(cfg.models.download_parts, 4);
        assert_eq!(cfg.models.selected.iter().count(), 0);
    }

    #[test]
    fn parses_models_section() {
        let cfg = Config::parse(
            "[models]\ndownload_parts = 2\n[models.selected]\ntriage = \"qwen3-4b-q4km\"\n",
        )
        .unwrap();
        assert_eq!(cfg.models.download_parts, 2);
        let sel: Vec<_> = cfg.models.selected.iter().collect();
        assert_eq!(sel, vec![(Tier::Triage, "qwen3-4b-q4km")]);
    }

    #[test]
    fn parses_runtime_ingest_and_retrieve_sections() {
        let cfg = Config::parse(
            "[runtime]\nenabled = false\nchat_model = \"qwen3-4b-q4km\"\nidle_stop_minutes = 0\n\
             [ingest]\nqueue_capacity = 8\n[retrieve]\nmmr = false\n",
        )
        .unwrap();
        assert!(!cfg.runtime.enabled);
        assert_eq!(cfg.runtime.chat_model.as_deref(), Some("qwen3-4b-q4km"));
        assert_eq!(cfg.runtime.embedding_model, None);
        assert_eq!(cfg.runtime.rerank_model, None);
        assert_eq!(cfg.runtime.idle_stop_minutes, 0);
        assert_eq!(cfg.ingest.queue_capacity, 8);
        assert!(!cfg.ingest.allow_cloud_embeddings);
        assert!(!cfg.retrieve.mmr);
        assert!(!cfg.retrieve.allow_cloud_embedding);
        let empty = Config::parse("").unwrap();
        assert_eq!(empty.runtime, RuntimeConfig::default());
        assert_eq!(empty.retrieve.reranker, None);
    }

    #[test]
    fn parses_the_rerank_model_and_an_external_reranker() {
        let cfg = Config::parse(
            "[runtime]\nrerank_model = \"bge-reranker-v2-m3-q4km\"\n\
             [retrieve]\nrerank_pool = 20\n[retrieve.reranker]\nbase_url = \"http://127.0.0.1:8013\"\n",
        )
        .unwrap();
        assert_eq!(
            cfg.runtime.rerank_model.as_deref(),
            Some("bge-reranker-v2-m3-q4km")
        );
        assert_eq!(cfg.retrieve.rerank_pool, 20);
        let r = cfg.retrieve.reranker.unwrap();
        assert_eq!(r.base_url, "http://127.0.0.1:8013");
        assert_eq!(r.timeout_ms, 2_000, "unset fields keep their defaults");
        for (text, needle) in [
            (
                "[runtime]\nrerank_model = \"Bad Id\"",
                "runtime.rerank_model",
            ),
            (
                "[retrieve.reranker]\ntimeout_ms = 5",
                "base_url is required",
            ),
            (
                "[retrieve.reranker]\nbase_url = \"not a url\"",
                "retrieve.reranker",
            ),
            (
                "[retrieve.reranker]\nbase_url = \"https://rerank.example.com\"",
                "not on this device",
            ),
        ] {
            let err = Config::parse(text).unwrap_err().to_string();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }

    #[test]
    fn rejects_invalid_configs_with_clear_errors() {
        let cases = [
            ("bogus = 1", "unknown field"),
            ("[models]\ndownload_parts = 0", "download_parts"),
            ("[models]\nparts = 2", "unknown field"),
            ("[models.selected]\nfast = \"x\"", "unknown field"),
            ("[models.selected]\ntriage = \"Bad Id\"", "not a model id"),
            ("[runtime]\nchat_model = \"../x\"", "runtime.chat_model"),
            ("[runtime]\nidle_stop_minutes = 99999", "idle_stop_minutes"),
            ("[runtime]\nbogus = 1", "unknown field"),
            (
                "[[providers]]\nid = \"a\"\nkind = \"anthropic\"\nmodel = \"m\"\napi_key = \"sk-plain\"",
                "api_key",
            ),
            (
                "[[providers]]\nid = \"a\"\nkind = \"openai_compatible\"\nmodel = \"m\"",
                "base_url is required",
            ),
            (
                "[[providers]]\nid = \"a\"\nkind = \"anthropic\"\nmodel = \"m\"\n[[providers]]\nid = \"a\"\nkind = \"anthropic\"\nmodel = \"m\"",
                "duplicate provider id",
            ),
            ("[[providers]]\nid = ", "expected"),
        ];
        for (text, needle) in cases {
            let err = Config::parse(text).unwrap_err().to_string();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }

    #[test]
    fn load_or_create_writes_default_once_and_names_the_file_on_error() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path().to_path_buf());
        let (cfg, created) = Config::load_or_create(&paths).unwrap();
        assert!(created);
        assert_eq!(cfg, Config::default_local());
        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            DEFAULT_CONFIG
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.config_file())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }

        let (_, created) = Config::load_or_create(&paths).unwrap();
        assert!(!created);

        std::fs::write(paths.config_file(), "[[providers]]\nid = 3\n").unwrap();
        let err = Config::load_or_create(&paths).unwrap_err().to_string();
        assert!(err.contains("config.toml"), "{err}");
        assert!(err.starts_with("invalid input: invalid config"), "{err}");
    }
}
