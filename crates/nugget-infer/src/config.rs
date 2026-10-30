//! Provider configuration (TOML) and secret resolution.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use nugget_core::capability::{Locality, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use url::Url;

use crate::locality::{HostResolver, Resolved, literal_locality, rank, resolve_url};
use crate::types::ProviderKind;

/// Keychain service name under which `keychain:NAME` secrets are stored (account =
/// [`provider_account`]`(NAME)`, i.e. `provider:NAME`). Shared by every crate via
/// [`nugget_core::secrets`].
pub use nugget_core::secrets::{KEYCHAIN_SERVICE, provider_account};

/// Default `base_url` per kind, used when the config leaves it out.
pub const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Anthropic API version sent by default (`anthropic-version` header).
pub const ANTHROPIC_DEFAULT_API_VERSION: &str = "2026-10-21";

/// Where an API key comes from. Plaintext keys in config files are deliberately not supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeyRef {
    /// `env:VAR`: read the environment variable `VAR`.
    Env(String),
    /// `keychain:NAME`: read the OS keychain entry service=[`KEYCHAIN_SERVICE`],
    /// account=[`provider_account`]`(NAME)` (`provider:NAME`).
    Keychain(String),
}

impl FromStr for ApiKeyRef {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let (scheme, name) = s.split_once(':').ok_or_else(|| {
            Error::Invalid(
                "api_key must be \"env:VAR\" or \"keychain:NAME\" (plaintext keys are not accepted)"
                    .into(),
            )
        })?;
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Invalid(format!("api_key {scheme}: name is empty")));
        }
        match scheme.trim() {
            "env" => Ok(ApiKeyRef::Env(name.to_string())),
            "keychain" => Ok(ApiKeyRef::Keychain(name.to_string())),
            other => Err(Error::Invalid(format!(
                "api_key scheme {other:?} is not supported; use \"env:VAR\" or \"keychain:NAME\""
            ))),
        }
    }
}

impl fmt::Display for ApiKeyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiKeyRef::Env(v) => write!(f, "env:{v}"),
            ApiKeyRef::Keychain(n) => write!(f, "keychain:{n}"),
        }
    }
}

impl Serialize for ApiKeyRef {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ApiKeyRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A resolved secret. `Debug` never prints the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Resolves [`ApiKeyRef`]s. Swap in a fake for tests.
pub trait SecretResolver: Send + Sync {
    fn resolve(&self, key: &ApiKeyRef) -> Result<Secret>;
}

/// Environment variables and the OS keychain (macOS Keychain, Windows Credential Manager, Secret
/// Service on Linux and the BSDs). Keychain entries use [`nugget_core::secrets`] naming.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSecrets;

impl SecretResolver for SystemSecrets {
    fn resolve(&self, key: &ApiKeyRef) -> Result<Secret> {
        match key {
            ApiKeyRef::Env(var) => match std::env::var(var) {
                Ok(v) if !v.trim().is_empty() => Ok(Secret(v.trim().to_string())),
                Ok(_) => Err(Error::NotFound(format!(
                    "environment variable {var} is empty"
                ))),
                Err(_) => Err(Error::NotFound(format!(
                    "environment variable {var} is not set"
                ))),
            },
            ApiKeyRef::Keychain(name) => {
                let name = name.clone();
                // Some keychain backends block on an async runtime internally; never call them
                // from a runtime thread.
                std::thread::scope(|s| {
                    s.spawn(|| read_keychain(&name))
                        .join()
                        .unwrap_or_else(|_| Err(Error::Other("keychain lookup panicked".into())))
                })
            }
        }
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd"
))]
fn read_keychain(name: &str) -> Result<Secret> {
    let account = provider_account(name);
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &account)
        .map_err(|e| Error::Other(format!("keychain {KEYCHAIN_SERVICE}/{account}: {e}")))?;
    match entry.get_password() {
        Ok(v) if !v.trim().is_empty() => Ok(Secret(v.trim().to_string())),
        Ok(_) => Err(Error::NotFound(format!(
            "keychain entry {KEYCHAIN_SERVICE}/{account} is empty"
        ))),
        Err(keyring::Error::NoEntry) => Err(Error::NotFound(format!(
            "keychain entry {KEYCHAIN_SERVICE}/{account} not found"
        ))),
        Err(e) => Err(Error::Other(format!(
            "keychain {KEYCHAIN_SERVICE}/{account}: {e}"
        ))),
    }
}

/// No OS keychain backend is built for this target.
#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd"
)))]
fn read_keychain(name: &str) -> Result<Secret> {
    Err(Error::Unsupported(format!(
        "keychain:{name}: no OS keychain on this platform; use env:VAR"
    )))
}

/// Which request field carries the output-token cap for `openai_compatible` servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensParam {
    /// `max_tokens` (Ollama, llama-server, LM Studio, vLLM, OpenRouter).
    #[default]
    MaxTokens,
    /// `max_completion_tokens` (OpenAI reasoning models).
    MaxCompletionTokens,
}

/// One configured provider. Model ids live only here (config), never in code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub id: ProviderId,
    pub kind: ProviderKind,
    /// API root. Anthropic: without `/v1` (default `https://api.anthropic.com`). OpenAI kinds:
    /// including the version prefix, e.g. `http://localhost:11434/v1` (default for
    /// `openai_responses`: `https://api.openai.com/v1`; required for `openai_compatible`).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Unix only: connect to this Unix-domain socket (absolute path) instead of the network.
    /// `base_url` then only supplies the scheme, the `Host` header and the path prefix (e.g.
    /// `http://localhost/v1`), and the endpoint is on-device. Rejected on other platforms.
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
    pub model: String,
    /// `env:VAR` or `keychain:NAME`. Omit for local servers that need no key.
    #[serde(default)]
    pub api_key: Option<ApiKeyRef>,
    /// Tiers this provider serves, in config order of preference across providers.
    #[serde(default)]
    pub tiers: Vec<Tier>,
    /// Treat this endpoint as further away than its host suggests, e.g. `cloud` for a localhost
    /// gateway that forwards to a cloud API. May only raise, never lower, the classification.
    #[serde(default)]
    pub locality: Option<Locality>,
    /// Context window in tokens, if known (reported in capabilities).
    #[serde(default)]
    pub context_tokens: Option<u32>,
    /// Output-token cap used when a request does not set one. Anthropic requires a value and
    /// falls back to 16000.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub connect_timeout_secs: Option<u64>,
    /// Abort a response after this many seconds without receiving bytes (default 600).
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    /// Retries for transient failures before the stream starts (default 2).
    #[serde(default)]
    pub max_retries: Option<u32>,
    /// Base backoff between retries in milliseconds (default 500, doubled per attempt).
    #[serde(default)]
    pub retry_base_ms: Option<u64>,
    /// Extra HTTP headers on every request (e.g. `anthropic-beta`, OpenRouter attribution).
    /// A value of the form `env:VAR` or `keychain:NAME` is resolved like `api_key` and sent as a
    /// sensitive header. Credential-bearing names (see [`is_sensitive_header`]) MUST use such a
    /// reference: plaintext secrets in config files are rejected.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// JSON object merged into every chat request body (top-level keys override ours).
    #[serde(default)]
    pub extra_body: Option<Value>,
    /// Anthropic only: `anthropic-version` header (default [`ANTHROPIC_DEFAULT_API_VERSION`]).
    #[serde(default)]
    pub anthropic_version: Option<String>,
    /// Anthropic only: set `eager_input_streaming` on tools (default true).
    #[serde(default)]
    pub eager_input_streaming: Option<bool>,
    /// OpenAI Responses only: let OpenAI store responses server-side (default false). When
    /// false, encrypted reasoning is requested so reasoning can be replayed statelessly.
    #[serde(default)]
    pub store: Option<bool>,
    /// OpenAI-compatible only: request usage in the stream (`stream_options.include_usage`,
    /// default true). Disable for servers that reject `stream_options`.
    #[serde(default)]
    pub stream_usage: Option<bool>,
    /// OpenAI-compatible only: name of the output-token cap field.
    #[serde(default)]
    pub max_tokens_param: Option<MaxTokensParam>,
    /// OpenAI-compatible only: texts per embeddings request (default 64).
    #[serde(default)]
    pub embed_batch_size: Option<usize>,
    /// OpenAI-compatible only: the server is llama.cpp's `llama-server`. Requests then carry
    /// `cache_prompt: true` (reuse the KV cache for the common prompt prefix), and
    /// [`crate::Provider::prewarm_prefix`] evaluates the stable prefix into the KV cache
    /// (`n_predict: 0`). Default: true for on-device endpoints (Ollama, LM Studio and vLLM ignore
    /// the extra fields), false otherwise.
    #[serde(default)]
    pub llama_cpp: Option<bool>,
    /// OpenAI-compatible llama-server only: send requests whose [`crate::CacheHint`] has a `key`
    /// to a fixed slot (`id_slot`, derived from the key; llama-server wraps it to its slot
    /// count). Default false: llama-server already picks the idle slot with the longest cached
    /// prefix match, whereas a pinned request waits whenever its slot is busy.
    #[serde(default)]
    pub slot_affinity: Option<bool>,
    /// OpenAI-compatible only: text prepended to search queries ([`crate::Provider::embed_query`]).
    /// Unset: the model's known default (see [`embed_prefixes`]); `""`: no prefix.
    #[serde(default)]
    pub embed_query_prefix: Option<String>,
    /// OpenAI-compatible only: text prepended to documents ([`crate::Provider::embed`]). Unset:
    /// the model's known default (see [`embed_prefixes`]); `""`: no prefix.
    #[serde(default)]
    pub embed_document_prefix: Option<String>,
}

/// Task prefixes for embedding models trained with them, as `(model substring, query prefix,
/// document prefix)`. Matched case-insensitively against the model name, so served variants
/// such as `nomic-embed-text-v1.5-q8` match too. Add a model (e.g. one with a query-only prefix)
/// with a row here.
const EMBED_PREFIXES: &[(&str, &str, &str)] =
    &[("nomic-embed-text", "search_query: ", "search_document: ")];

/// The `(query, document)` embedding prefixes for `cfg`: the configured
/// `embed_query_prefix`/`embed_document_prefix` where set, else the model's known default from a
/// small table, else none.
pub fn embed_prefixes(cfg: &ProviderConfig) -> (String, String) {
    let model = cfg.model.to_ascii_lowercase();
    let (query, document) = EMBED_PREFIXES
        .iter()
        .find(|(name, _, _)| model.contains(name))
        .map_or(("", ""), |&(_, q, d)| (q, d));
    (
        cfg.embed_query_prefix
            .clone()
            .unwrap_or_else(|| query.to_owned()),
        cfg.embed_document_prefix
            .clone()
            .unwrap_or_else(|| document.to_owned()),
    )
}

/// A TOML file holding `[[providers]]` tables.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProvidersFile {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
}

impl ProvidersFile {
    /// Parse and validate. Provider ids must be unique.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        let file: ProvidersFile =
            toml::from_str(s).map_err(|e| Error::Invalid(format!("providers config: {e}")))?;
        let mut seen = std::collections::HashSet::new();
        for p in &file.providers {
            p.validate()?;
            if !seen.insert(p.id.clone()) {
                return Err(Error::Invalid(format!("duplicate provider id {}", p.id)));
            }
        }
        Ok(file)
    }
}

impl ProviderConfig {
    /// Parse a single provider table.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        let cfg: ProviderConfig =
            toml::from_str(s).map_err(|e| Error::Invalid(format!("provider config: {e}")))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// The effective base URL.
    pub fn base_url(&self) -> Result<Url> {
        let raw = match (&self.base_url, self.kind) {
            (Some(u), _) => u.as_str(),
            (None, ProviderKind::Anthropic) => ANTHROPIC_DEFAULT_BASE_URL,
            (None, ProviderKind::OpenaiResponses) => OPENAI_DEFAULT_BASE_URL,
            (None, ProviderKind::OpenaiCompatible) => {
                return Err(Error::Invalid(format!(
                    "provider {}: base_url is required for openai_compatible",
                    self.id
                )));
            }
        };
        let url = Url::parse(raw)
            .map_err(|e| Error::Invalid(format!("provider {}: base_url {raw:?}: {e}", self.id)))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Error::Invalid(format!(
                "provider {}: base_url must be http(s)",
                self.id
            )));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::Invalid(format!(
                "provider {}: credentials in base_url are not allowed; use api_key",
                self.id
            )));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(Error::Invalid(format!(
                "provider {}: base_url must not have a query or fragment",
                self.id
            )));
        }
        Ok(url)
    }

    /// Locality classified from the host WITHOUT DNS, raised by the optional `locality`
    /// override. Only `localhost` and IP literals classify below `Cloud` here; any other host
    /// name reports `Cloud` until a built provider resolves it (see [`crate::locality`] and
    /// [`crate::ProviderMeta::locality`]).
    pub fn effective_locality(&self) -> Result<Locality> {
        let url = self.base_url()?;
        if self.socket_path.is_some() {
            return self.apply_locality_override(Locality::OnDevice, &url);
        }
        match literal_locality(&url)? {
            Some(classified) => self.apply_locality_override(classified, &url),
            None => Ok(Locality::Cloud),
        }
    }

    /// Raise `classified` by the `locality` override; refuse an override that would lower it.
    fn apply_locality_override(&self, classified: Locality, url: &Url) -> Result<Locality> {
        match self.locality {
            None => Ok(classified),
            Some(o) if rank(o) >= rank(classified) => Ok(o),
            Some(o) => Err(Error::Invalid(format!(
                "provider {}: locality {o:?} would hide egress; {} classifies as {classified:?}",
                self.id, url
            ))),
        }
    }

    /// Resolve the base URL's host once (see [`crate::locality`]) and return the effective
    /// locality plus the resolution to pin the client to. Refuses plaintext http to anything that
    /// is (or could not be shown not to be) an internet host.
    pub(crate) fn resolve_network(
        &self,
        resolver: &dyn HostResolver,
    ) -> Result<(Locality, Resolved)> {
        let url = self.base_url()?;
        if self.socket_path.is_some() {
            // A Unix socket on this machine: no name to resolve, nothing to pin.
            let locality = self.apply_locality_override(Locality::OnDevice, &url)?;
            let resolved = Resolved {
                locality: Locality::OnDevice,
                pin: None,
                unresolved: false,
            };
            return Ok((locality, resolved));
        }
        let resolved = resolve_url(&url, resolver)?;
        if url.scheme() == "http" && resolved.locality == Locality::Cloud {
            let why = if resolved.unresolved {
                " (it did not resolve, so it is treated as one)"
            } else {
                ""
            };
            return Err(Error::Invalid(format!(
                "provider {}: refusing plaintext http to internet host {}{why}",
                self.id,
                url.host_str().unwrap_or_default()
            )));
        }
        let locality = self.apply_locality_override(resolved.locality, &url)?;
        Ok((locality, resolved))
    }

    /// Check the config for mistakes without touching the network or secrets.
    pub fn validate(&self) -> Result<()> {
        let id = &self.id;
        if id.as_str().trim().is_empty() {
            return Err(Error::Invalid("provider id is empty".into()));
        }
        if self.model.trim().is_empty() {
            return Err(Error::Invalid(format!("provider {id}: model is empty")));
        }
        if self.tiers.contains(&Tier::Rerank) {
            return Err(Error::Invalid(format!(
                "provider {id}: `rerank` is not a routing tier; point search at a reranker with \
                 [retrieve.reranker] or set [runtime] rerank_model"
            )));
        }
        let url = self.base_url()?;
        if let Some(socket) = &self.socket_path {
            if !cfg!(unix) {
                return Err(Error::Unsupported(format!(
                    "provider {id}: socket_path (Unix-domain sockets) is only supported on Unix"
                )));
            }
            if !socket.is_absolute() {
                return Err(Error::Invalid(format!(
                    "provider {id}: socket_path must be an absolute path"
                )));
            }
            // Traffic stays on this machine whatever the URL's host says.
            self.apply_locality_override(Locality::OnDevice, &url)?;
        } else if let Some(classified) = literal_locality(&url)? {
            // Host names are classified (and plaintext http to them refused) only once they are
            // resolved, when the provider is built. Literals are checked here.
            self.apply_locality_override(classified, &url)?;
            if url.scheme() == "http" && classified == Locality::Cloud {
                return Err(Error::Invalid(format!(
                    "provider {id}: refusing plaintext http to internet host {}",
                    url.host_str().unwrap_or_default()
                )));
            }
        }
        for (name, value) in &self.headers {
            if is_sensitive_header(name) && value.parse::<ApiKeyRef>().is_err() {
                return Err(Error::Invalid(format!(
                    "provider {id}: header {name:?} carries a credential; set it to \"env:VAR\" \
                     or \"keychain:NAME\" (plaintext secrets are not accepted)"
                )));
            }
        }
        if self.max_output_tokens == Some(0) {
            return Err(Error::Invalid(format!(
                "provider {id}: max_output_tokens must be > 0"
            )));
        }
        if self.embed_batch_size == Some(0) {
            return Err(Error::Invalid(format!(
                "provider {id}: embed_batch_size must be > 0"
            )));
        }
        if let Some(extra) = &self.extra_body
            && !extra.is_object()
        {
            return Err(Error::Invalid(format!(
                "provider {id}: extra_body must be a table"
            )));
        }
        let wrong_kind = |field: &str, kind: ProviderKind| -> Result<()> {
            Err(Error::Invalid(format!(
                "provider {id}: `{field}` only applies to kind {kind}"
            )))
        };
        if self.kind != ProviderKind::Anthropic {
            if self.anthropic_version.is_some() {
                wrong_kind("anthropic_version", ProviderKind::Anthropic)?;
            }
            if self.eager_input_streaming.is_some() {
                wrong_kind("eager_input_streaming", ProviderKind::Anthropic)?;
            }
        }
        if self.kind != ProviderKind::OpenaiResponses && self.store.is_some() {
            wrong_kind("store", ProviderKind::OpenaiResponses)?;
        }
        if self.kind != ProviderKind::OpenaiCompatible {
            if self.stream_usage.is_some() {
                wrong_kind("stream_usage", ProviderKind::OpenaiCompatible)?;
            }
            if self.max_tokens_param.is_some() {
                wrong_kind("max_tokens_param", ProviderKind::OpenaiCompatible)?;
            }
            if self.embed_batch_size.is_some() {
                wrong_kind("embed_batch_size", ProviderKind::OpenaiCompatible)?;
            }
            if self.llama_cpp.is_some() {
                wrong_kind("llama_cpp", ProviderKind::OpenaiCompatible)?;
            }
            if self.slot_affinity.is_some() {
                wrong_kind("slot_affinity", ProviderKind::OpenaiCompatible)?;
            }
        }
        Ok(())
    }
}

/// Header names that carry credentials and so may only be set from `env:VAR`/`keychain:NAME` in
/// [`ProviderConfig::headers`]: `authorization`, `proxy-authorization`, `cookie`, `x-api-key`,
/// `api-key`, and any name containing `auth`, `key`, `token`, `secret`, `password`, `cookie`,
/// `session` or `credential` (case-insensitive).
pub fn is_sensitive_header(name: &str) -> bool {
    const EXACT: [&str; 6] = [
        "authorization",
        "proxy-authorization",
        "cookie",
        "set-cookie",
        "x-api-key",
        "api-key",
    ];
    const PARTS: [&str; 9] = [
        "auth",
        "key",
        "token",
        "secret",
        "password",
        "passwd",
        "cookie",
        "session",
        "credential",
    ];
    let n = name.trim().to_ascii_lowercase();
    EXACT.contains(&n.as_str()) || PARTS.iter().any(|p| n.contains(p))
}

/// Resolve `cfg.headers`: `env:VAR`/`keychain:NAME` values through `secrets`, everything else
/// verbatim. Every value is marked sensitive so it never appears in logs.
pub(crate) fn resolve_headers(
    cfg: &ProviderConfig,
    secrets: &dyn SecretResolver,
) -> Result<Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)>> {
    use reqwest::header::{HeaderName, HeaderValue};
    let mut out = Vec::with_capacity(cfg.headers.len());
    for (k, v) in &cfg.headers {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| Error::Invalid(format!("provider {}: header name {k:?}: {e}", cfg.id)))?;
        let mut value = match v.parse::<ApiKeyRef>() {
            Ok(r) => {
                let secret = secrets.resolve(&r).map_err(|e| match e {
                    Error::NotFound(m) => {
                        Error::NotFound(format!("provider {}: header {k} {r}: {m}", cfg.id))
                    }
                    other => other,
                })?;
                HeaderValue::from_str(secret.expose()).map_err(|_| {
                    Error::Invalid(format!(
                        "provider {}: header {k} {r}: value has characters not allowed in an HTTP header",
                        cfg.id
                    ))
                })?
            }
            Err(_) if is_sensitive_header(k) => {
                return Err(Error::Invalid(format!(
                    "provider {}: header {k:?} must be \"env:VAR\" or \"keychain:NAME\"",
                    cfg.id
                )));
            }
            Err(_) => HeaderValue::from_str(v).map_err(|e| {
                Error::Invalid(format!("provider {}: header {k:?} value: {e}", cfg.id))
            })?,
        };
        value.set_sensitive(true);
        out.push((name, value));
    }
    Ok(out)
}

/// Shallow-merge `extra` into the request body object.
pub(crate) fn merge_extra_body(body: &mut Value, extra: Option<&Value>) {
    if let (Some(Value::Object(dst)), Some(Value::Object(src))) = (Some(body), extra) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(toml: &str) -> Result<ProviderConfig> {
        ProviderConfig::from_toml_str(toml)
    }

    #[test]
    fn embed_prefix_defaults_and_overrides() {
        let base = "id='e'\nkind='openai_compatible'\nbase_url='http://localhost:1/v1'\n";
        let prefixes = |extra: &str| embed_prefixes(&cfg(&format!("{base}{extra}")).unwrap());
        let nomic = ("search_query: ".to_owned(), "search_document: ".to_owned());
        assert_eq!(prefixes("model='nomic-embed-text-v1.5-q8'"), nomic);
        assert_eq!(prefixes("model='Nomic-Embed-Text:latest'"), nomic);
        assert_eq!(
            prefixes("model='other-embed'"),
            (String::new(), String::new())
        );
        assert_eq!(
            prefixes("model='nomic-embed-text-v1.5'\nembed_query_prefix=''"),
            (String::new(), "search_document: ".to_owned())
        );
        assert_eq!(
            prefixes("model='other'\nembed_query_prefix='q: '\nembed_document_prefix='d: '"),
            ("q: ".to_owned(), "d: ".to_owned())
        );
    }

    #[test]
    fn api_key_refs() {
        assert_eq!(
            "env:FOO".parse::<ApiKeyRef>().unwrap(),
            ApiKeyRef::Env("FOO".into())
        );
        assert_eq!(
            "keychain:anthropic".parse::<ApiKeyRef>().unwrap(),
            ApiKeyRef::Keychain("anthropic".into())
        );
        assert!("sk-ant-plaintext".parse::<ApiKeyRef>().is_err());
        assert!("file:/x".parse::<ApiKeyRef>().is_err());
        assert!("env:".parse::<ApiKeyRef>().is_err());
        assert_eq!(ApiKeyRef::Env("X".into()).to_string(), "env:X");
    }

    #[test]
    fn parses_minimal_and_defaults() {
        let c = cfg(r#"
            id = "a"
            kind = "anthropic"
            model = "m"
            api_key = "env:K"
            tiers = ["synthesis", "agentic"]
        "#)
        .unwrap();
        assert_eq!(c.base_url().unwrap().as_str(), "https://api.anthropic.com/");
        assert_eq!(c.effective_locality().unwrap(), Locality::Cloud);
        assert_eq!(c.tiers, vec![Tier::Synthesis, Tier::Agentic]);
        assert_eq!(c.api_key, Some(ApiKeyRef::Env("K".into())));
    }

    #[test]
    fn the_rerank_tier_is_not_a_provider_tier() {
        let err = cfg(r#"
            id = "a"
            kind = "openai_compatible"
            base_url = "http://127.0.0.1:8080/v1"
            model = "m"
            tiers = ["light", "rerank"]
        "#)
        .unwrap_err();
        assert!(err.to_string().contains("rerank"), "{err}");
    }

    #[test]
    fn rejects_bad_configs() {
        // unknown field
        assert!(cfg("id='a'\nkind='anthropic'\nmodel='m'\nbogus=1").is_err());
        // plaintext key
        assert!(cfg("id='a'\nkind='anthropic'\nmodel='m'\napi_key='sk-123'").is_err());
        // openai_compatible needs base_url
        assert!(cfg("id='a'\nkind='openai_compatible'\nmodel='m'").is_err());
        // plaintext http to an internet IP literal (names are checked when built, after DNS)
        assert!(
            cfg("id='a'\nkind='openai_compatible'\nmodel='m'\nbase_url='http://8.8.8.8/v1'")
                .is_err()
        );
        // credentials in URL
        assert!(
            cfg(
                "id='a'\nkind='openai_compatible'\nmodel='m'\nbase_url='https://u:p@example.com/v1'"
            )
            .is_err()
        );
        // locality override may not hide egress (for names this is checked after DNS, at build)
        assert!(
            cfg("id='a'\nkind='anthropic'\nmodel='m'\nbase_url='https://8.8.8.8'\nlocality='on_device'")
                .is_err()
        );
        // kind-specific option on the wrong kind
        assert!(
            cfg("id='a'\nkind='openai_compatible'\nmodel='m'\nbase_url='http://localhost:1/v1'\nanthropic_version='x'")
                .is_err()
        );
        assert!(cfg("id='a'\nkind='anthropic'\nmodel='m'\nstore=true").is_err());
        assert!(cfg("id='a'\nkind='anthropic'\nmodel='m'\nextra_body=1").is_err());
        assert!(cfg("id=''\nkind='anthropic'\nmodel='m'").is_err());
    }

    #[test]
    fn locality_override_can_only_raise() {
        let c = cfg("id='gw'\nkind='openai_compatible'\nmodel='m'\nbase_url='http://127.0.0.1:4000/v1'\nlocality='cloud'").unwrap();
        assert_eq!(c.effective_locality().unwrap(), Locality::Cloud);
        let c = cfg(
            "id='o'\nkind='openai_compatible'\nmodel='m'\nbase_url='http://localhost:11434/v1'",
        )
        .unwrap();
        assert_eq!(c.effective_locality().unwrap(), Locality::OnDevice);
        let c = cfg(
            "id='lan'\nkind='openai_compatible'\nmodel='m'\nbase_url='http://192.168.1.4:11434/v1'",
        )
        .unwrap();
        assert_eq!(c.effective_locality().unwrap(), Locality::LocalNetwork);
    }

    #[test]
    fn unix_socket_endpoints_are_on_device_and_unix_only() {
        let base = "id='l'\nkind='openai_compatible'\nmodel='m'\nbase_url='http://localhost/v1'\n";
        let res = cfg(&format!("{base}socket_path='/run/nugget/chat.sock'"));
        #[cfg(unix)]
        {
            let c = res.unwrap();
            assert_eq!(
                c.socket_path.as_deref(),
                Some(std::path::Path::new("/run/nugget/chat.sock"))
            );
            assert_eq!(c.effective_locality().unwrap(), Locality::OnDevice);
            // Even a URL that names an internet host: the socket decides where traffic goes.
            let c = cfg("id='l'\nkind='openai_compatible'\nmodel='m'\nbase_url='http://example.com/v1'\nsocket_path='/s.sock'").unwrap();
            assert_eq!(c.effective_locality().unwrap(), Locality::OnDevice);
            let (loc, resolved) = c.resolve_network(&crate::locality::SystemResolver).unwrap();
            assert_eq!((loc, resolved.pin), (Locality::OnDevice, None));
            // The override may still raise it.
            let c = cfg(&format!("{base}socket_path='/s.sock'\nlocality='cloud'")).unwrap();
            assert_eq!(c.effective_locality().unwrap(), Locality::Cloud);
            // Relative paths are refused.
            assert!(cfg(&format!("{base}socket_path='chat.sock'")).is_err());
        }
        #[cfg(not(unix))]
        assert!(matches!(res, Err(Error::Unsupported(_))), "{res:?}");
    }

    #[test]
    fn credential_headers_must_be_references() {
        let base = "id='a'\nkind='anthropic'\nmodel='m'\n";
        for name in [
            "authorization",
            "Authorization",
            "x-api-key",
            "api-key",
            "proxy-authorization",
            "cookie",
            "x-goog-api-key",
            "x-auth-token",
            "openai-organization-secret",
        ] {
            assert!(is_sensitive_header(name), "{name}");
            let plain = format!("{base}headers = {{ \"{name}\" = \"sk-plaintext\" }}");
            let err = cfg(&plain).unwrap_err();
            assert!(err.to_string().contains("credential"), "{name}: {err}");
            for r in ["env:MY_KEY", "keychain:gw"] {
                cfg(&format!("{base}headers = {{ \"{name}\" = \"{r}\" }}")).unwrap();
            }
        }
        for name in ["anthropic-beta", "X-Title", "HTTP-Referer", "user-agent"] {
            assert!(!is_sensitive_header(name), "{name}");
        }
        cfg(&format!("{base}headers = {{ \"X-Title\" = \"Nugget\" }}")).unwrap();
    }

    #[test]
    fn header_references_resolve_to_sensitive_values() {
        struct Fake;
        impl SecretResolver for Fake {
            fn resolve(&self, key: &ApiKeyRef) -> Result<Secret> {
                match key {
                    ApiKeyRef::Env(v) if v == "GW_TOKEN" => Ok(Secret::new("Bearer abc")),
                    ApiKeyRef::Keychain(n) if n == "cookie" => Ok(Secret::new("sid=1")),
                    _ => Err(Error::NotFound("unset".into())),
                }
            }
        }
        let c = cfg(r#"
            id = "a"
            kind = "anthropic"
            model = "m"
            headers = { "proxy-authorization" = "env:GW_TOKEN", "cookie" = "keychain:cookie", "X-Title" = "Nugget" }
        "#)
        .unwrap();
        let h = resolve_headers(&c, &Fake).unwrap();
        let get = |n: &str| h.iter().find(|(k, _)| k.as_str() == n).unwrap().1.clone();
        assert_eq!(get("proxy-authorization").to_str().unwrap(), "Bearer abc");
        assert_eq!(get("cookie").to_str().unwrap(), "sid=1");
        assert_eq!(get("x-title").to_str().unwrap(), "Nugget");
        assert!(h.iter().all(|(_, v)| v.is_sensitive()));
        let mut missing = c.clone();
        missing
            .headers
            .insert("x-api-key".into(), "env:NOPE".into());
        let err = resolve_headers(&missing, &Fake).unwrap_err();
        assert!(
            matches!(&err, Error::NotFound(m) if m.contains("x-api-key")),
            "{err}"
        );
    }

    #[test]
    fn providers_file_rejects_duplicates() {
        let s = r#"
            [[providers]]
            id = "x"
            kind = "anthropic"
            model = "m"
            [[providers]]
            id = "x"
            kind = "openai_responses"
            model = "m"
        "#;
        assert!(ProvidersFile::from_toml_str(s).is_err());
    }

    #[test]
    fn example_file_is_valid() {
        let s = include_str!("../examples/providers.example.toml");
        let file = ProvidersFile::from_toml_str(s).unwrap();
        assert!(file.providers.len() >= 4);
        for kind in [
            ProviderKind::Anthropic,
            ProviderKind::OpenaiResponses,
            ProviderKind::OpenaiCompatible,
        ] {
            assert!(file.providers.iter().any(|p| p.kind == kind), "{kind}");
        }
        assert!(
            file.providers
                .iter()
                .any(|p| p.tiers.contains(&Tier::Embedding))
        );
    }

    #[test]
    fn env_secret_resolution() {
        // PATH exists on every OS (Windows env lookups are case-insensitive).
        let s = SystemSecrets
            .resolve(&ApiKeyRef::Env("PATH".into()))
            .unwrap();
        assert!(!s.expose().is_empty());
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        let e = SystemSecrets
            .resolve(&ApiKeyRef::Env("NUGGET_TEST_DEFINITELY_UNSET_VAR".into()))
            .unwrap_err();
        assert!(matches!(e, Error::NotFound(_)));
    }

    #[test]
    fn merge_extra() {
        let mut body = serde_json::json!({"a": 1, "b": 2});
        merge_extra_body(&mut body, Some(&serde_json::json!({"b": 3, "c": 4})));
        assert_eq!(body, serde_json::json!({"a": 1, "b": 3, "c": 4}));
    }
}
