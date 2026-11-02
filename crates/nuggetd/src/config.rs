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
pub const DEFAULT_CONFIG: &str = r#"# Heed daemon (nuggetd) configuration.
#
# Heed is local-first. Out of the box every tier below is served by the on-device runtime
# (a model running on this machine), so nothing leaves the device until you add a cloud provider.
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
# configure below that run on your network, or on this device once probed successfully, are tried
# first; unprobed on-device providers come after the runtime. Cloud providers come last and are
# never used for triage or (by default) embeddings.
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
# HTTP limits: connections served at once (others wait), milliseconds a connection gets to send a
# request's headers (also the idle keep-alive limit), and milliseconds a request gets for its body.
# max_connections = 64
# header_timeout_ms = 10000
# body_timeout_ms = 30000

# --- On-device: Ollama (uncomment to use) --------------------------------------------------------
# Install Ollama (https://ollama.com), then pull the model, e.g. `ollama pull qwen3:4b`.
# [[providers]]
# id = "local"
# kind = "openai_compatible"
# base_url = "http://127.0.0.1:11434/v1"
# model = "qwen3:4b"
# tiers = ["triage", "light", "synthesis"]

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
    /// install, `<data>/vault` when Heed runs with `--home`/`NUGGET_HOME`.
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
    /// Most HTTP connections served at once; further ones wait in the listen backlog
    /// (default 64, 1..=[`MAX_MCP_CONNECTIONS`]).
    pub max_connections: usize,
    /// Milliseconds an HTTP connection gets to send a complete request head; an idle keep-alive
    /// connection is closed after this too (default 10000, 1..=[`MAX_MCP_TIMEOUT_MS`]).
    pub header_timeout_ms: u64,
    /// Milliseconds an HTTP request gets to send its whole body (default 30000,
    /// 1..=[`MAX_MCP_TIMEOUT_MS`]).
    pub body_timeout_ms: u64,
}

/// Upper bound for `[mcp] max_connections`.
pub const MAX_MCP_CONNECTIONS: usize = 4096;
/// Upper bound for `[mcp] header_timeout_ms` and `body_timeout_ms` (10 minutes).
pub const MAX_MCP_TIMEOUT_MS: u64 = 600_000;

impl Default for McpSection {
    fn default() -> Self {
        Self {
            enabled: true,
            http: true,
            port: DEFAULT_MCP_PORT,
            allowed_origins: Vec::new(),
            max_connections: nugget_mcp::http::DEFAULT_MAX_CONNECTIONS,
            header_timeout_ms: nugget_mcp::http::DEFAULT_HEADER_TIMEOUT_MS,
            body_timeout_ms: nugget_mcp::http::DEFAULT_BODY_TIMEOUT_MS,
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
    /// Manifest id of a cross-encoder reranker, an entry with the `rerank` tier (served as
    /// `local-rerank`, used by search only, never routed). Unset (default): no local reranker;
    /// there is no automatic recommendation. `[retrieve.reranker]` takes precedence.
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
        if !(1..=MAX_MCP_CONNECTIONS).contains(&self.mcp.max_connections) {
            return Err(Error::Invalid(format!(
                "mcp.max_connections must be between 1 and {MAX_MCP_CONNECTIONS}"
            )));
        }
        for (key, v) in [
            ("header_timeout_ms", self.mcp.header_timeout_ms),
            ("body_timeout_ms", self.mcp.body_timeout_ms),
        ] {
            if !(1..=MAX_MCP_TIMEOUT_MS).contains(&v) {
                return Err(Error::Invalid(format!(
                    "mcp.{key} must be between 1 and {MAX_MCP_TIMEOUT_MS}"
                )));
            }
        }
        Ok(())
    }

    /// Read `path`, or write [`DEFAULT_CONFIG`] there first if it does not exist. Returns the
    /// config and whether it was just created. Errors name the file.
    pub fn load_or_create_at(path: &Path) -> Result<(Self, bool)> {
        Self::load_at(path, true)
    }

    /// [`Config::load_or_create_at`], but an old default provider to migrate is only dropped in
    /// memory, never rewritten in the file: for one-shot commands that run beside the daemon
    /// (backup, restore), which must not race its own config writes.
    pub fn load_or_create_no_rewrite(paths: &Paths) -> Result<(Self, bool)> {
        Self::load_at(&paths.config_file(), false)
    }

    fn load_at(path: &Path, rewrite: bool) -> Result<(Self, bool)> {
        let cannot_read = |e| Error::Invalid(format!("cannot read {}: {e}", path.display()));
        let (text, created) = match std::fs::read_to_string(path) {
            Ok(text) => (text, false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                write_default(path)?;
                (std::fs::read_to_string(path).map_err(cannot_read)?, true)
            }
            Err(e) => return Err(cannot_read(e)),
        };
        let (text, skip) = if created {
            (text, None)
        } else {
            migrate_file(path, text, rewrite)
        };
        let mut cfg = Self::parse(&text).map_err(|e| {
            Error::Invalid(format!("invalid config {}: {}", path.display(), msg(e)))
        })?;
        if let Some(i) = skip
            && cfg
                .providers
                .get(i)
                .is_some_and(|p| p.id.as_str() == "local")
        {
            cfg.providers.remove(i);
        }
        Ok((cfg, created))
    }

    /// [`Config::load_or_create_at`] on `Paths::config_file()`.
    pub fn load_or_create(paths: &Paths) -> Result<(Self, bool)> {
        Self::load_or_create_at(&paths.config_file())
    }
}

/// The `[[providers]]` block that builds before the on-device runtime wrote, enabled, into every
/// new config: an Ollama server most users never installed (GG6-4). Compared as TOML values.
const OLD_DEFAULT_OLLAMA: &str = r#"id = "local"
kind = "openai_compatible"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen3:4b"
tiers = ["triage", "light", "synthesis"]
"#;

/// In every config written by builds whose default comments the Ollama example out; never in
/// the files of the builds that enabled it.
const CURRENT_DEFAULT_MARKER: &str = "# --- On-device: Ollama (uncomment to use)";

/// Written above the commented-out block by [`migrate_old_default_provider`].
const MIGRATION_NOTE: [&str; 2] = [
    "# Turned off by Heed: earlier versions enabled this example Ollama provider by default.",
    "# Remove the leading \"# \" from the lines below if you do run Ollama with this model.",
];

/// What [`migrate_old_default_provider`] decided.
#[derive(Debug, PartialEq, Eq)]
enum Migration {
    /// Nothing to migrate (or the text does not parse; [`Config::parse`] reports that).
    Unchanged,
    /// The new file contents: the old default block commented out, every other byte kept.
    Rewritten(String),
    /// The old default is provider `[i]`, but rewriting the text safely was not possible: leave
    /// the file alone and drop the provider in memory.
    SkipInMemory(usize),
}

/// Find an untouched old default Ollama provider ([`OLD_DEFAULT_OLLAMA`], nothing changed or
/// added) and comment it out. Any provider the user added or edited is left alone. Idempotent:
/// a migrated file has no such block left.
fn migrate_old_default_provider(text: &str) -> Migration {
    // One shot: a file this migration already touched, or one written by a build whose default
    // only offers the block commented out, keeps an enabled block on purpose (the user turned
    // it on, as the note above it says).
    if text.contains(MIGRATION_NOTE[0]) || text.contains(CURRENT_DEFAULT_MARKER) {
        return Migration::Unchanged;
    }
    let Some(idx) = old_default_index(text) else {
        return Migration::Unchanged;
    };
    let Ok(doc) = toml::from_str::<toml::Table>(text) else {
        return Migration::Unchanged;
    };
    let count = doc
        .get("providers")
        .and_then(toml::Value::as_array)
        .map_or(0, Vec::len);
    match comment_out_provider(text, count, idx) {
        Some(new) => {
            // Accept the rewrite only if it removes exactly that provider and nothing else.
            let mut expected = doc.clone();
            if let Some(toml::Value::Array(a)) = expected.get_mut("providers") {
                a.remove(idx);
                if a.is_empty() {
                    expected.remove("providers");
                }
            }
            match toml::from_str::<toml::Table>(&new) {
                Ok(got) if got == expected => Migration::Rewritten(new),
                _ => Migration::SkipInMemory(idx),
            }
        }
        None => Migration::SkipInMemory(idx),
    }
}

/// `text` with the `idx`-th of `count` `[[providers]]` blocks commented out line by line (header
/// and settings; comments and blank lines kept), preceded by [`MIGRATION_NOTE`]. `None` when the
/// block headers cannot be matched to the providers one to one.
fn comment_out_provider(text: &str, count: usize, idx: usize) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let setting = |l: &str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    };
    let is_providers_header = |l: &str| {
        let t = l.split('#').next().unwrap_or("");
        let compact: String = t.chars().filter(|c| !c.is_whitespace()).collect();
        compact == "[[providers]]"
    };
    let headers: Vec<usize> = (0..lines.len())
        .filter(|&i| is_providers_header(lines[i]))
        .collect();
    if headers.len() != count {
        return None;
    }
    let start = *headers.get(idx)?;
    let end = (start + 1..lines.len())
        .find(|&i| lines[i].trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let eol = if lines[start].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut out = String::with_capacity(text.len() + 200);
    for l in &lines[..start] {
        out.push_str(l);
    }
    for note in MIGRATION_NOTE {
        out.push_str(note);
        out.push_str(eol);
    }
    for l in &lines[start..end] {
        if setting(l) {
            out.push_str("# ");
        }
        out.push_str(l);
    }
    for l in &lines[end..] {
        out.push_str(l);
    }
    Some(out)
}

/// Apply [`migrate_old_default_provider`] to the config file at `path` whose contents are `text`.
/// Returns the text to parse and, when the file could not be rewritten, the index of the
/// provider to drop in memory.
fn migrate_file(path: &Path, text: String, rewrite: bool) -> (String, Option<usize>) {
    // A symlinked config (dotfile managers) would be replaced by a plain file: leave it alone.
    let symlink = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    let migration = match migrate_old_default_provider(&text) {
        Migration::Rewritten(_) if symlink || !rewrite => match old_default_index(&text) {
            Some(i) => Migration::SkipInMemory(i),
            None => Migration::Unchanged,
        },
        m => m,
    };
    match migration {
        Migration::Unchanged => (text, None),
        Migration::Rewritten(new) => match write_replacing(path, new.as_bytes()) {
            Ok(()) => {
                tracing::info!(
                    path = %path.display(),
                    "config migrated: commented out provider `local` (Ollama at 127.0.0.1:11434), \
                     the unchanged example that earlier versions enabled by default; uncomment it \
                     to use Ollama"
                );
                (new, None)
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "config migration: cannot rewrite the file; ignoring provider `local` (the \
                     unchanged Ollama example that earlier versions enabled by default) for now"
                );
                let idx = old_default_index(&text);
                (text, idx)
            }
        },
        Migration::SkipInMemory(i) => {
            tracing::warn!(
                path = %path.display(),
                "config migration: ignoring provider `local` (the unchanged Ollama example that \
                 earlier versions enabled by default); comment it out in the file to silence this"
            );
            (text, Some(i))
        }
    }
}

/// Index of the untouched old default provider in `text`, if any.
fn old_default_index(text: &str) -> Option<usize> {
    let doc = toml::from_str::<toml::Table>(text).ok()?;
    let old: toml::Table = toml::from_str(OLD_DEFAULT_OLLAMA).ok()?;
    doc.get("providers")?
        .as_array()?
        .iter()
        .position(|p| p.as_table().is_some_and(|t| *t == old))
}

/// Replace `path` atomically (temp file in the same directory, then rename), keeping its
/// permissions.
fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".into());
    let tmp = dir.join(format!(".{name}.{}.migrate.tmp", std::process::id()));
    let perms = std::fs::metadata(path)?.permissions();
    let res = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.set_permissions(perms)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
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
    t.as_str()
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

    #[test]
    fn default_config_has_no_providers_and_no_secrets() {
        for cfg in [Config::default_local(), Config::parse("").unwrap()] {
            assert!(cfg.providers.is_empty());
            assert_eq!(cfg.runtime, RuntimeConfig::default());
            assert!(cfg.runtime.enabled);
            assert_eq!(cfg.mcp, McpSection::default());
        }
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

    #[test]
    fn rejects_invalid_configs_with_clear_errors() {
        let cases = [
            ("bogus = 1", "unknown field"),
            ("[models]\ndownload_parts = 0", "download_parts"),
            ("[models.selected]\nfast = \"x\"", "unknown field"),
            ("[models.selected]\ntriage = \"Bad Id\"", "not a model id"),
            ("[runtime]\nchat_model = \"../x\"", "runtime.chat_model"),
            (
                "[runtime]\nrerank_model = \"Bad Id\"",
                "runtime.rerank_model",
            ),
            ("[runtime]\nidle_stop_minutes = 99999", "idle_stop_minutes"),
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
            ("[mcp]\nmax_connections = 0", "max_connections"),
            ("[mcp]\nmax_connections = 4097", "max_connections"),
            ("[mcp]\nheader_timeout_ms = 600001", "header_timeout_ms"),
            ("[mcp]\nbody_timeout_ms = \"slow\"", "body_timeout_ms"),
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
        assert!(Config::parse("[mcp]\nmax_connections = 4096\nbody_timeout_ms = 600000\n").is_ok());
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

    /// A config written by an earlier build: the Ollama example enabled.
    fn old_default_config() -> String {
        let commented = "# [[providers]]\n# id = \"local\"\n# kind = \"openai_compatible\"\n# base_url = \"http://127.0.0.1:11434/v1\"\n# model = \"qwen3:4b\"\n# tiers = [\"triage\", \"light\", \"synthesis\"]\n";
        assert!(DEFAULT_CONFIG.contains(commented));
        let enabled = commented.replace("# ", "");
        // Earlier builds' header for the block, without "(uncomment to use)".
        assert!(DEFAULT_CONFIG.contains(CURRENT_DEFAULT_MARKER));
        DEFAULT_CONFIG.replacen(commented, &enabled, 1).replacen(
            CURRENT_DEFAULT_MARKER,
            "# --- On-device: Ollama",
            1,
        )
    }

    #[test]
    fn migration_comments_out_the_untouched_old_default() {
        let old = old_default_config();
        assert_eq!(Config::parse(&old).unwrap().providers.len(), 1);
        let Migration::Rewritten(new) = migrate_old_default_provider(&old) else {
            panic!("not migrated");
        };
        assert!(Config::parse(&new).unwrap().providers.is_empty());
        // Every other byte is kept: dropping the note and the added "# " gives the old file.
        let mut restored = new.clone();
        for note in MIGRATION_NOTE {
            restored = restored.replacen(&format!("{note}\n"), "", 1);
        }
        let commented = "# [[providers]]\n# id = \"local\"\n# kind = \"openai_compatible\"\n# base_url = \"http://127.0.0.1:11434/v1\"\n# model = \"qwen3:4b\"\n# tiers = [\"triage\", \"light\", \"synthesis\"]\n";
        assert_eq!(restored.matches(commented).count(), 1, "{new}");
        let first = restored.find(commented).unwrap();
        restored.replace_range(first..first + commented.len(), &commented.replace("# ", ""));
        assert_eq!(restored, old);
        // Idempotent.
        assert_eq!(migrate_old_default_provider(&new), Migration::Unchanged);
        assert_eq!(
            migrate_old_default_provider(DEFAULT_CONFIG),
            Migration::Unchanged
        );
    }

    /// KK1 review: the migration is one shot. A block the user turned back on (after the note,
    /// or in a config from a build whose default only offers it commented out) stays on.
    #[test]
    fn a_block_the_user_turned_back_on_stays_on() {
        let Migration::Rewritten(migrated) = migrate_old_default_provider(&old_default_config())
        else {
            panic!("not migrated");
        };
        let commented = "# [[providers]]\n# id = \"local\"\n# kind = \"openai_compatible\"\n# base_url = \"http://127.0.0.1:11434/v1\"\n# model = \"qwen3:4b\"\n# tiers = [\"triage\", \"light\", \"synthesis\"]\n";
        let enabled = commented.replace("# ", "");
        // The user follows the note and turns the migrated block back on.
        let turned_on = migrated.replacen(commented, &enabled, 1);
        assert!(turned_on.contains(MIGRATION_NOTE[0]));
        assert_eq!(Config::parse(&turned_on).unwrap().providers.len(), 1);
        assert_eq!(
            migrate_old_default_provider(&turned_on),
            Migration::Unchanged
        );

        let current = DEFAULT_CONFIG.replacen(commented, &enabled, 1);
        assert_ne!(
            current, DEFAULT_CONFIG,
            "the current default still has the example"
        );
        assert_eq!(Config::parse(&current).unwrap().providers.len(), 1);
        assert_eq!(migrate_old_default_provider(&current), Migration::Unchanged);
    }

    /// KK1 review: a symlinked config is never replaced by a plain file; the old default is
    /// only ignored in memory.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.toml");
        let link = dir.path().join("config.toml");
        std::fs::write(&real, old_default_config()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (cfg, created) = Config::load_or_create_at(&link).unwrap();
        assert!(!created);
        assert!(cfg.providers.iter().all(|p| p.id.as_str() != "local"));
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            old_default_config()
        );
    }

    #[test]
    fn migration_leaves_edited_or_user_added_providers_alone() {
        let old = old_default_config();
        let variants = [
            old.replace("model = \"qwen3:4b\"", "model = \"qwen3:8b\""),
            old.replace("id = \"local\"\nkind", "id = \"ollama\"\nkind"),
            old.replace(
                "tiers = [\"triage\", \"light\", \"synthesis\"]\n",
                "tiers = [\"triage\", \"light\", \"synthesis\"]\n[providers.headers]\nx-a = \"b\"\n",
            ),
            // A user's own Ollama provider with another model.
            "[[providers]]\nid = \"local\"\nkind = \"openai_compatible\"\nbase_url = \"http://127.0.0.1:11434/v1\"\nmodel = \"llama3.2\"\n".to_string(),
        ];
        for v in variants {
            assert_ne!(v, old);
            assert_eq!(Config::parse(&v).unwrap().providers.len(), 1, "{v}");
            assert_eq!(
                migrate_old_default_provider(&v),
                Migration::Unchanged,
                "{v}"
            );
        }
    }

    #[test]
    fn load_migrates_the_file_once_and_skips_in_memory_when_text_is_unusual() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, old_default_config()).unwrap();
        let (cfg, created) = Config::load_or_create_at(&path).unwrap();
        assert!(!created);
        assert!(cfg.providers.is_empty());
        let migrated = std::fs::read_to_string(&path).unwrap();
        assert!(migrated.contains(MIGRATION_NOTE[0]));
        let (cfg, _) = Config::load_or_create_at(&path).unwrap();
        assert!(cfg.providers.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), migrated);
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1);

        // Written as an inline array: the text is left alone, the provider is skipped.
        let inline = "providers = [{ id = \"local\", kind = \"openai_compatible\", base_url = \"http://127.0.0.1:11434/v1\", model = \"qwen3:4b\", tiers = [\"triage\", \"light\", \"synthesis\"] }]\n";
        std::fs::write(&path, inline).unwrap();
        assert_eq!(
            migrate_old_default_provider(inline),
            Migration::SkipInMemory(0)
        );
        let (cfg, _) = Config::load_or_create_at(&path).unwrap();
        assert!(cfg.providers.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), inline);
    }
}
