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
use nugget_retrieve::RetrieveConfig;
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
# Stop a server after this many idle minutes (0 = keep running); it restarts on the next request.
idle_stop_minutes = 30

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
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
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
        ] {
            if let Some(id) = id
                && !is_model_id(id)
            {
                return Err(Error::Invalid(format!(
                    "runtime.{field}: `{id}` is not a model id"
                )));
            }
        }
        if self.runtime.idle_stop_minutes > 7 * 24 * 60 {
            return Err(Error::Invalid(
                "runtime.idle_stop_minutes must be at most 10080 (one week)".into(),
            ));
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
        assert_eq!(cfg.runtime.idle_stop_minutes, 0);
        assert_eq!(cfg.ingest.queue_capacity, 8);
        assert!(!cfg.ingest.allow_cloud_embeddings);
        assert!(!cfg.retrieve.mmr);
        assert!(!cfg.retrieve.allow_cloud_embedding);
        let empty = Config::parse("").unwrap();
        assert_eq!(empty.runtime, RuntimeConfig::default());
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
