//! "Bring your own key": check, save, replace and remove a cloud (or self-hosted) provider key,
//! routing to it at once ([`State::set_provider`]; `restart_required` is always `false`).
//!
//! - `ai.key.test` lists the provider's models and classifies the answer (`ok`, `bad_key`,
//!   `wrong_url`, `no_network`, `server_error`) as a result, never an RPC error.
//! - `ai.key.save` checks, stores the key in the [`SecretStore`] under [`provider_account`] and
//!   appends a `[[providers]]` table referring to `keychain:NAME`; or, with `key_ref: "env:VAR"`
//!   (the no-keychain fallback), to the variable. A taken id answers `exists`.
//! - `ai.key.update`/`ai.key.remove` edit only that provider's table, keeping every other byte.
//! - `ai.local.detect` asks the usual local servers (Ollama, LM Studio) on this computer for
//!   their models. A server on a loopback address may be saved without a key.
//! - With `light_model`, `ai.key.save` also writes a `NAME-light` table (same key and address)
//!   serving the `light` tier; update and remove treat the pair as one provider.
//!
//! Every request is in the egress ledger (feature `ai_setup`, loopback too). The key is never
//! logged, echoed or written anywhere but the secret store. Only `https`, except loopback hosts.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::SecretStore;
use nugget_connector::http::MeteredHttp;
use nugget_core::capability::{Locality, Tier};
use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
use nugget_core::secrets::provider_account;
use nugget_core::{Error, Result};
use nugget_infer::{ApiKeyRef, ProviderConfig, ProviderKind, Secret, SecretResolver};
use nugget_ipc::{Notifier, RpcError, parse_params, to_result};
use nugget_store::AuditEntry;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Feature, FeatureContext};
use crate::config::{Config, DEFAULT_CONFIG};
use crate::daemon::{State, blocking};

/// IPC methods.
pub(crate) const AI_KEY_TEST: &str = "ai.key.test";
pub(crate) const AI_KEY_SAVE: &str = "ai.key.save";
pub(crate) const AI_KEY_UPDATE: &str = "ai.key.update";
pub(crate) const AI_KEY_REMOVE: &str = "ai.key.remove";
pub(crate) const AI_KEY_SAVED: &str = "ai.key.saved";
pub(crate) const AI_LOCAL_DETECT: &str = "ai.local.detect";
const METHODS: &[&str] = &[
    AI_KEY_TEST,
    AI_KEY_SAVE,
    AI_KEY_UPDATE,
    AI_KEY_REMOVE,
    AI_KEY_SAVED,
    AI_LOCAL_DETECT,
];

/// Egress ledger feature name.
const FEATURE: &str = "ai_setup";
/// The whole check (connect, request, body) must finish within this.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// Cap on the model-list body.
const MAX_BODY: usize = 4 * 1024 * 1024;
/// Cap on the model ids returned.
const MAX_MODELS: usize = 500;

const OPENAI_DEFAULT_BASE: &str = "https://api.openai.com/v1";
const ANTHROPIC_DEFAULT_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Local servers `ai.local.detect` asks: `(id, label, base URL)`.
const LOCAL_SERVERS: &[(&str, &str, &str)] = &[
    ("ollama", "Ollama", "http://127.0.0.1:11434/v1"),
    ("lmstudio", "LM Studio", "http://127.0.0.1:1234/v1"),
];
/// How long a local server may take to list its models.
const DETECT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Suffix of the companion table that serves the `light` tier with `light_model`.
const LIGHT_SUFFIX: &str = "-light";

/// First line of the comment written above each table `ai.key.save` appends.
const MARKER: &str = "# Added by Nugget's AI setup.";

/// Which API the provider speaks (the `kind` of a `[[providers]]` table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    OpenaiCompatible,
    OpenaiResponses,
    Anthropic,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::OpenaiCompatible => "openai_compatible",
            Kind::OpenaiResponses => "openai_responses",
            Kind::Anthropic => "anthropic",
        }
    }

    fn of(kind: ProviderKind) -> Self {
        match kind {
            ProviderKind::Anthropic => Kind::Anthropic,
            ProviderKind::OpenaiResponses => Kind::OpenaiResponses,
            ProviderKind::OpenaiCompatible => Kind::OpenaiCompatible,
        }
    }
}

/// An API key; never printed.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub(crate) struct ApiKey(String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// `ai.key.test` params: `api_key` or `key_ref` (`env:VAR`).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct KeyTestRequest {
    pub kind: Kind,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<ApiKey>,
    #[serde(default)]
    pub key_ref: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

/// `ai.key.save` params: `api_key` or `key_ref` (`env:VAR`).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct KeySaveRequest {
    #[serde(alias = "id")]
    pub name: String,
    pub kind: Kind,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<ApiKey>,
    #[serde(default)]
    pub key_ref: Option<String>,
    pub model: String,
    /// A cheaper model for the `light` tier (a `NAME-light` table); unset or equal to `model`:
    /// `model` serves every tier.
    #[serde(default)]
    pub light_model: Option<String>,
}

/// `ai.key.update` params: the provider, and a new `api_key` or `key_ref`; `base_url` replaces
/// the address when given.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct KeyUpdateRequest {
    #[serde(alias = "id")]
    pub name: String,
    #[serde(default)]
    pub api_key: Option<ApiKey>,
    #[serde(default)]
    pub key_ref: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

/// `ai.key.remove` params.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct KeyRemoveRequest {
    #[serde(alias = "id")]
    pub name: String,
}

/// Why a check (or a save) did not succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    Ok,
    BadKey,
    WrongUrl,
    NoNetwork,
    ServerError,
    /// `ai.key.save` only: a provider with this id is already configured.
    Exists,
    /// The OS keychain cannot store the key (for example no Secret Service on Linux); use an
    /// environment variable (`key_ref: "env:VAR"`) instead.
    NoKeychain,
    /// `key_ref` names an environment variable the daemon does not have (or that is empty).
    EnvMissing,
    /// `ai.key.update` / `ai.key.remove`: no provider with this id in config.toml.
    NotFound,
    /// `ai.key.update` / `ai.key.remove`: the provider does not use a key (a local server) or is
    /// managed by Nugget.
    NotEditable,
    /// `ai.key.save`: the name belongs to Nugget itself (`local-*`, or a provider the daemon has
    /// that is not in config.toml, such as an on-device runtime slot).
    Reserved,
}

/// `ai.key.test` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct KeyTestResult {
    pub ok: bool,
    pub reason: Reason,
    pub message: String,
    pub models: Vec<String>,
}

impl KeyTestResult {
    fn fail(reason: Reason, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            reason,
            message: message.into(),
            models: Vec::new(),
        }
    }
}

/// `ai.key.save` and `ai.key.update` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct KeySaveResult {
    pub ok: bool,
    pub reason: Reason,
    pub message: String,
    pub models: Vec<String>,
    pub saved: bool,
    pub provider_id: Option<String>,
    /// Always `false`: the provider is routed at once. Kept for older clients.
    pub restart_required: bool,
    /// With `no_keychain`: an environment variable name to suggest for `key_ref`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_env: Option<String>,
}

impl KeySaveResult {
    fn from_test(t: KeyTestResult) -> Self {
        Self {
            ok: t.ok,
            reason: t.reason,
            message: t.message,
            models: t.models,
            saved: false,
            provider_id: None,
            restart_required: false,
            suggested_env: None,
        }
    }

    fn fail(reason: Reason, message: impl Into<String>) -> Self {
        Self::from_test(KeyTestResult::fail(reason, message))
    }
}

/// `ai.key.remove` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct KeyRemoveResult {
    pub ok: bool,
    pub reason: Reason,
    pub message: String,
    pub removed: bool,
    pub provider_id: String,
}

/// One entry of `ai.key.saved`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedProvider {
    pub id: String,
    pub kind: Kind,
    pub base_url: Option<String>,
    pub model: String,
    /// `keychain` or `env`.
    pub key: String,
    /// With `key: "env"`: the variable's name.
    pub env_var: Option<String>,
    /// The `light` tier's model (its `NAME-light` table), if it has its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_model: Option<String>,
    /// How far the provider is (`on_device`, `local_network`, `cloud`).
    pub locality: String,
    /// Routed right now (its key could be read and the provider built).
    pub available: bool,
    /// Why it is not available.
    pub error: Option<String>,
}

/// `ai.key.saved` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedList {
    pub providers: Vec<SavedProvider>,
}

/// One local server `ai.local.detect` found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct LocalServer {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub models: Vec<String>,
    /// Models that look local but forward prompts to a cloud service (Ollama's `-cloud`
    /// models); saved with `locality = "cloud"`.
    pub cloud_models: Vec<String>,
    /// A provider in config.toml already uses this address.
    pub configured: bool,
}

/// `ai.local.detect` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct LocalDetect {
    pub servers: Vec<LocalServer>,
}

/// Reads one environment variable of the daemon (swapped in tests).
pub(crate) type EnvReader = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

fn process_env() -> EnvReader {
    Arc::new(|name| std::env::var(name).ok())
}

/// The `ai_keys` feature.
pub(crate) fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    let egress: Arc<dyn EgressSink> = Arc::new(cx.state.store.clone());
    Ok(Arc::new(AiKeys {
        state: cx.state.clone(),
        http: metered(egress.clone())?,
        egress,
        env: process_env(),
        write: tokio::sync::Mutex::new(()),
    }))
}

/// The HTTP client for checks: no redirects, egress recorded under [`FEATURE`].
pub(crate) fn metered(egress: Arc<dyn EgressSink>) -> Result<MeteredHttp> {
    Ok(MeteredHttp::new(egress, FEATURE)?.with_max_body(MAX_BODY))
}

struct AiKeys {
    state: Arc<State>,
    http: MeteredHttp,
    /// For providers built after a save.
    egress: Arc<dyn EgressSink>,
    env: EnvReader,
    /// One config write at a time.
    write: tokio::sync::Mutex<()>,
}

#[async_trait]
impl Feature for AiKeys {
    fn name(&self) -> &'static str {
        "ai_keys"
    }

    fn methods(&self) -> &'static [&'static str] {
        METHODS
    }

    async fn call(&self, method: &str, params: Value, _: Notifier) -> Result<Value, RpcError> {
        match method {
            AI_KEY_TEST => {
                let req: KeyTestRequest = parse_params(params)?;
                let spec = key_spec(req.api_key.as_ref(), req.key_ref.as_deref())
                    .and_then(|s| keyless_allowed(s, req.kind, req.base_url.as_deref()))
                    .map_err(RpcError::invalid_params)?;
                let key = match spec.read(&*self.env, None) {
                    Ok(k) => k,
                    Err(r) => return to_result(r),
                };
                let check = Check {
                    kind: req.kind,
                    base_url: req.base_url,
                    key,
                    model: req.model,
                };
                let result = check_key(&self.http, &check, CHECK_TIMEOUT).await;
                self.flush_egress().await;
                to_result(result)
            }
            AI_KEY_SAVE => {
                let req: KeySaveRequest = parse_params(params)?;
                validate_save(&req).map_err(RpcError::invalid_params)?;
                let spec = key_spec(req.api_key.as_ref(), req.key_ref.as_deref())
                    .and_then(|s| keyless_allowed(s, req.kind, req.base_url.as_deref()))
                    .map_err(RpcError::invalid_params)?;
                let result = self.save(req, spec).await?;
                to_result(result)
            }
            AI_KEY_UPDATE => {
                let req: KeyUpdateRequest = parse_params(params)?;
                validate_name(&req.name).map_err(RpcError::invalid_params)?;
                let spec = key_spec(req.api_key.as_ref(), req.key_ref.as_deref())
                    .and_then(|s| keyless_allowed(s, Kind::OpenaiResponses, None))
                    .map_err(RpcError::invalid_params)?;
                let result = self.update(req, spec).await?;
                to_result(result)
            }
            AI_KEY_REMOVE => {
                let req: KeyRemoveRequest = parse_params(params)?;
                validate_name(&req.name).map_err(RpcError::invalid_params)?;
                let result = self.remove(&req.name).await?;
                to_result(result)
            }
            AI_KEY_SAVED => to_result(self.saved().await?),
            AI_LOCAL_DETECT => {
                let path = self.state.paths.config_file();
                let bases: Vec<String> = blocking(move || read_providers(&path))
                    .await?
                    .into_iter()
                    .filter_map(|c| c.base_url)
                    .collect();
                let servers = detect_local(&self.http, LOCAL_SERVERS, &bases, DETECT_TIMEOUT).await;
                self.flush_egress().await;
                to_result(LocalDetect { servers })
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

impl AiKeys {
    /// Make the ledger row visible to `egress.recent` right away.
    async fn flush_egress(&self) {
        let store = self.state.store.clone();
        let _ = blocking(move || {
            store.flush_egress();
            Ok(())
        })
        .await;
    }

    async fn audit(&self, action: &str, target: &str, detail: Value) {
        let store = self.state.store.clone();
        let audit = AuditEntry {
            actor: "user".into(),
            action: action.into(),
            target: target.into(),
            detail,
        };
        if let Err(e) = blocking(move || store.append_audit(&audit)).await {
            tracing::warn!(error = %e, "cannot append audit entry");
        }
    }

    async fn save(&self, req: KeySaveRequest, spec: KeySpec) -> Result<KeySaveResult> {
        let config_path = self.state.paths.config_file();
        let light_model = light_model(&req);
        let names: Vec<String> = std::iter::once(req.name.clone())
            .chain(light_model.as_ref().map(|_| light_name(&req.name)))
            .collect();
        // Refuse a taken id before going to the network (checked again when writing).
        for n in &names {
            if provider_exists(&config_path, n)? {
                return Ok(exists_result(n));
            }
        }
        if names.iter().any(|n| {
            reserved(n, |id| {
                self.state.router.provider(&ProviderId::new(id)).is_some()
                    || self.state.slot(&ProviderId::new(id)).is_some()
            })
        }) {
            return Ok(KeySaveResult::fail(
                Reason::Reserved,
                format!(
                    "Nugget uses the name \"{}\" for its own models. Pick another name.",
                    req.name
                ),
            ));
        }
        let key = match spec.read(&*self.env, None) {
            Ok(k) => k,
            Err(r) => return Ok(KeySaveResult::from_test(r)),
        };
        let checked = self
            .check(req.kind, req.base_url.clone(), key.as_deref(), &req.model)
            .await;
        if !checked.ok {
            return Ok(KeySaveResult::from_test(checked));
        }
        let _guard = self.write.lock().await;
        let secrets = self.state.secrets.clone();
        let base_url = clean_base(req.base_url.as_deref());
        let model = req.model.trim().to_owned();
        let entry = NewProvider {
            name: req.name.clone(),
            kind: req.kind,
            cloud_forwarder: is_cloud_forwarder(base_url.as_deref(), &model)
                || light_model
                    .as_deref()
                    .is_some_and(|m| is_cloud_forwarder(base_url.as_deref(), m)),
            base_url,
            model,
            light_model,
            key: spec.stored_ref(&req.name),
        };
        let written = {
            let entry = entry.clone();
            let path = config_path.clone();
            let key = key.map(|k| k.trim().to_owned());
            blocking(move || write_provider(&path, &*secrets, &entry, key.as_deref())).await?
        };
        match written {
            Written::Saved => {}
            Written::Exists => return Ok(exists_result(&req.name)),
            Written::NoKeychain => return Ok(no_keychain_result(&req.name)),
        }
        let detail = json!({
            "kind": entry.kind.as_str(), "base_url": entry.base_url,
            "model": entry.model, "light_model": entry.light_model,
            "key": entry.key.as_ref().map_or("none", KeyRef::kind),
        });
        let msg = if entry.key.is_some() {
            "Key checked and saved. You can use it now."
        } else {
            "Server checked and saved. You can use it now."
        };
        Ok(self
            .saved_ok("ai.key.saved", entry.name, detail, msg, checked.models)
            .await)
    }

    /// Check `key` against the provider and make the ledger row visible right away.
    async fn check(
        &self,
        kind: Kind,
        base_url: Option<String>,
        key: Option<&str>,
        model: &str,
    ) -> KeyTestResult {
        let check = Check {
            kind,
            base_url,
            key: key.map(str::to_owned),
            model: Some(model.to_owned()),
        };
        let checked = check_key(&self.http, &check, CHECK_TIMEOUT).await;
        self.flush_egress().await;
        checked
    }

    /// Log, audit and route a provider whose key was just written.
    async fn saved_ok(
        &self,
        action: &str,
        name: String,
        detail: Value,
        message: &str,
        models: Vec<String>,
    ) -> KeySaveResult {
        tracing::info!(provider = %name, action, "provider key written");
        self.audit(action, &name, detail).await;
        self.route(&name).await;
        self.route(&light_name(&name)).await;
        KeySaveResult {
            ok: true,
            reason: Reason::Ok,
            message: message.into(),
            models,
            saved: true,
            provider_id: Some(name),
            restart_required: false,
            suggested_env: None,
        }
    }

    async fn update(&self, req: KeyUpdateRequest, spec: KeySpec) -> Result<KeySaveResult> {
        let config_path = self.state.paths.config_file();
        let _guard = self.write.lock().await;
        let current = {
            let path = config_path.clone();
            let name = req.name.clone();
            blocking(move || find_provider(&path, &name)).await?
        };
        let cfg = match editable(current.as_ref(), &req.name).and_then(|c| {
            // A keyless local server has no key to replace.
            c.api_key.as_ref().map(|_| c).ok_or(Reason::NotEditable)
        }) {
            Ok(c) => c.clone(),
            Err(reason) => {
                return Ok(KeySaveResult::fail(
                    reason,
                    not_editable_message(reason, &req.name),
                ));
            }
        };
        // The variable the provider already uses stays usable, at its own address only.
        let current_env = match (&cfg.api_key, &req.base_url) {
            (Some(ApiKeyRef::Env(v)), None) => Some(v.as_str()),
            _ => None,
        };
        let key = match spec.read(&*self.env, current_env) {
            Ok(Some(k)) => k,
            Ok(None) => return Ok(KeySaveResult::fail(Reason::BadKey, "Enter an API key.")),
            Err(r) => return Ok(KeySaveResult::from_test(r)),
        };
        let base_url = match req.base_url.as_deref() {
            Some(b) => clean_base(Some(b)),
            None => cfg.base_url.clone(),
        };
        let kind = Kind::of(cfg.kind);
        let checked = self
            .check(kind, base_url.clone(), Some(&key), &cfg.model)
            .await;
        if !checked.ok {
            return Ok(KeySaveResult::from_test(checked));
        }
        let secrets = self.state.secrets.clone();
        let (path, name) = (config_path.clone(), req.name.clone());
        let Some(new_ref) = spec.stored_ref(&req.name) else {
            return Ok(KeySaveResult::fail(Reason::BadKey, "Enter an API key."));
        };
        let change = KeyChange {
            old: cfg.api_key.clone(),
            new: new_ref.clone(),
            base_url: req.base_url.as_ref().map(|_| base_url.clone()),
        };
        let written =
            blocking(move || replace_key(&path, &*secrets, &name, &change, key.trim().to_owned()))
                .await?;
        if written == Written::NoKeychain {
            return Ok(no_keychain_result(&req.name));
        }
        let detail = json!({
            "kind": kind.as_str(), "base_url": base_url,
            "model": cfg.model, "key": new_ref.kind(),
        });
        let msg = "Key checked and replaced. Nugget uses the new key now.";
        Ok(self
            .saved_ok("ai.key.updated", req.name, detail, msg, checked.models)
            .await)
    }

    async fn remove(&self, name: &str) -> Result<KeyRemoveResult> {
        let config_path = self.state.paths.config_file();
        let _guard = self.write.lock().await;
        let current = {
            let (path, name) = (config_path.clone(), name.to_owned());
            blocking(move || find_provider(&path, &name)).await?
        };
        let cfg = match editable(current.as_ref(), name) {
            Ok(c) => c.clone(),
            Err(reason) => {
                return Ok(KeyRemoveResult {
                    ok: false,
                    reason,
                    message: not_editable_message(reason, name),
                    removed: false,
                    provider_id: name.to_owned(),
                });
            }
        };
        let secrets = self.state.secrets.clone();
        let (path, owned) = (config_path.clone(), name.to_owned());
        let key = cfg.api_key.clone();
        let removed =
            blocking(move || remove_provider(&path, &*secrets, &owned, key.as_ref())).await?;
        for id in removed {
            self.state.remove_provider(&ProviderId::new(id));
        }
        tracing::info!(provider = %name, "provider removed");
        self.audit(
            "ai.key.removed",
            name,
            json!({ "kind": Kind::of(cfg.kind).as_str(), "model": cfg.model }),
        )
        .await;
        Ok(KeyRemoveResult {
            ok: true,
            reason: Reason::Ok,
            message: if cfg.api_key.is_some() {
                format!("\"{name}\" was removed and its key deleted.")
            } else {
                format!("\"{name}\" was removed.")
            },
            removed: true,
            provider_id: name.to_owned(),
        })
    }

    async fn saved(&self) -> Result<SavedList> {
        let path = self.state.paths.config_file();
        let configs = blocking(move || read_providers(&path)).await?;
        let providers = configs
            .iter()
            .filter(|c| editable(Some(c), c.id.as_str()).is_ok())
            .filter(|c| companion_of(&configs, c).is_none())
            .map(|c| {
                let (key, env_var) = match &c.api_key {
                    Some(ApiKeyRef::Env(v)) => ("env", Some(v.clone())),
                    Some(ApiKeyRef::Keychain(_)) => ("keychain", None),
                    None => ("none", None),
                };
                let light = light_name(c.id.as_str());
                let light_model = configs
                    .iter()
                    .find(|o| o.id.as_str() == light && companion_of(&configs, o).is_some())
                    .map(|o| o.model.clone());
                let locality = self
                    .state
                    .router
                    .provider(&c.id)
                    .map(|p| p.meta().locality)
                    .or(c.locality)
                    .or_else(|| {
                        c.base_url
                            .as_deref()
                            .and_then(scheme_and_host)
                            .filter(|(_, h)| is_loopback(h))
                            .map(|_| Locality::OnDevice)
                    })
                    .unwrap_or(Locality::Cloud);
                let locality = serde_json::to_value(locality)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let error = self.state.unavailable_reason(&c.id);
                let available = error.is_none() && self.state.router.provider(&c.id).is_some();
                SavedProvider {
                    id: c.id.to_string(),
                    kind: Kind::of(c.kind),
                    base_url: c.base_url.clone(),
                    model: c.model.clone(),
                    key: key.into(),
                    env_var,
                    light_model,
                    locality,
                    available,
                    error: error.or_else(|| {
                        (!available).then(|| "Not in use yet: restart Nugget.".to_owned())
                    }),
                }
            })
            .collect();
        Ok(SavedList { providers })
    }

    /// Build provider `name` from config.toml as it is now and route to it (or record why it
    /// cannot be built). The key is read from the daemon's secret store or environment.
    async fn route(&self, name: &str) {
        let path = self.state.paths.config_file();
        let resolver = StoreResolver {
            secrets: self.state.secrets.clone(),
            env: self.env.clone(),
        };
        let (egress, owned) = (self.egress.clone(), name.to_owned());
        let built = blocking(move || {
            let Some(cfg) = find_provider(&path, &owned)? else {
                return Ok(None);
            };
            let p = nugget_infer::build_provider_with(&cfg, egress, &resolver)
                .map_err(|e| e.to_string());
            Ok(Some((cfg, p)))
        })
        .await;
        match built {
            Ok(Some((cfg, p))) => {
                if let Err(e) = &p {
                    tracing::warn!(provider = %name, error = %e, "saved provider cannot be built");
                }
                self.state.set_provider(cfg, p);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(provider = %name, error = %e, "cannot route saved provider"),
        }
    }
}

/// Resolves `keychain:NAME` from the daemon's secret store (the one `ai.key.save` wrote to) and
/// `env:VAR` from the daemon's environment.
struct StoreResolver {
    secrets: Arc<dyn SecretStore>,
    env: EnvReader,
}

impl SecretResolver for StoreResolver {
    fn resolve(&self, key: &ApiKeyRef) -> Result<Secret> {
        let value = match key {
            ApiKeyRef::Keychain(name) => self.secrets.get(&provider_account(name))?,
            ApiKeyRef::Env(var) => (self.env)(var),
        };
        match value.map(|v| v.trim().to_owned()) {
            Some(v) if !v.is_empty() => Ok(Secret::new(v)),
            _ => Err(Error::NotFound(format!("{key} is not set"))),
        }
    }
}

fn exists_result(name: &str) -> KeySaveResult {
    KeySaveResult::fail(
        Reason::Exists,
        format!(
            "A provider named \"{name}\" is already set up. Replace its key or pick another name."
        ),
    )
}

fn no_keychain_result(name: &str) -> KeySaveResult {
    let var = suggested_env(name);
    KeySaveResult {
        suggested_env: Some(var.clone()),
        ..KeySaveResult::fail(
            Reason::NoKeychain,
            format!(
                "This computer has no keyring to store the key safely. Put the key in an environment variable (for example {var}) and enter its name instead."
            ),
        )
    }
}

fn not_editable_message(reason: Reason, name: &str) -> String {
    match reason {
        Reason::NotFound => format!("There is no provider named \"{name}\"."),
        _ => format!("\"{name}\" does not use an API key, so there is no key to change or remove."),
    }
}

/// An environment variable name to suggest for provider `name`, e.g. `NUGGET_OPENAI_KEY`.
fn suggested_env(name: &str) -> String {
    let upper: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("NUGGET_{upper}_KEY")
}

/// The provider in config.toml if `ai.key.update` / `ai.key.remove` may change it: it refers to
/// a key, or is a server on this computer (keyless; it can be removed, not given a key), and is
/// not one of the runtime's own (`local-*`).
fn editable<'a>(
    cfg: Option<&'a ProviderConfig>,
    name: &str,
) -> std::result::Result<&'a ProviderConfig, Reason> {
    let cfg = cfg.ok_or(Reason::NotFound)?;
    let loopback = cfg
        .base_url
        .as_deref()
        .and_then(scheme_and_host)
        .is_some_and(|(_, h)| is_loopback(h));
    if name.starts_with("local-") || (cfg.api_key.is_none() && !loopback) {
        return Err(Reason::NotEditable);
    }
    Ok(cfg)
}

/// The companion table's id for provider `name`: `NAME-light`.
fn light_name(name: &str) -> String {
    format!("{name}{LIGHT_SUFFIX}")
}

/// The provider whose `light` companion `c` is: `c` is `NAME-light`, provider `NAME` exists with
/// the same key and address, and `c` serves only the `light` tier.
fn companion_of<'a>(all: &'a [ProviderConfig], c: &ProviderConfig) -> Option<&'a ProviderConfig> {
    let main = c.id.as_str().strip_suffix(LIGHT_SUFFIX)?;
    (c.tiers == [Tier::Light])
        .then(|| {
            all.iter().find(|m| {
                m.id.as_str() == main && m.api_key == c.api_key && m.base_url == c.base_url
            })
        })
        .flatten()
}

/// `light_model` when it is set and differs from `model`.
fn light_model(req: &KeySaveRequest) -> Option<String> {
    req.light_model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty() && *m != req.model.trim())
        .map(str::to_owned)
}

/// Whether `model` at `base` looks local but forwards prompts to a cloud service: Ollama's
/// `…-cloud` / `…:cloud` models on a loopback server.
fn is_cloud_forwarder(base: Option<&str>, model: &str) -> bool {
    let loopback = base
        .and_then(scheme_and_host)
        .is_some_and(|(_, h)| is_loopback(h));
    let m = model.to_ascii_lowercase();
    loopback && (m.ends_with("-cloud") || m.ends_with(":cloud"))
}

/// Ask each local server for its models (no key, ledgered like every check); the ones that
/// answered with a model list, with `configured` set when `configured_bases` has the address.
pub(crate) async fn detect_local(
    http: &MeteredHttp,
    servers: &[(&str, &str, &str)],
    configured_bases: &[String],
    timeout: Duration,
) -> Vec<LocalServer> {
    let checks = servers.iter().map(|&(id, label, base)| async move {
        let check = Check {
            kind: Kind::OpenaiCompatible,
            base_url: Some(base.to_owned()),
            key: None,
            model: None,
        };
        let r = check_key(http, &check, timeout).await;
        r.ok.then(|| {
            let (cloud_models, models) = r
                .models
                .into_iter()
                .partition(|m| is_cloud_forwarder(Some(base), m));
            let same =
                |b: &String| b.trim_end_matches('/').replace("localhost", "127.0.0.1") == base;
            LocalServer {
                id: id.to_owned(),
                label: label.to_owned(),
                base_url: base.to_owned(),
                models,
                cloud_models,
                configured: configured_bases.iter().any(same),
            }
        })
    });
    futures::future::join_all(checks)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Whether `ai.key.save` must not take `name` (not in config.toml): Nugget's own `local-*`
/// names, or one the daemon already routes (`known`).
fn reserved(name: &str, known: impl Fn(&str) -> bool) -> bool {
    name.starts_with("local-") || known(name)
}

fn validate_name(n: &str) -> std::result::Result<(), String> {
    if n.is_empty()
        || n.len() > 32
        || !n
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(
            "name must be 1 to 32 characters: lowercase letters, digits, \"_\" or \"-\"".into(),
        );
    }
    Ok(())
}

/// Name and model checks for `ai.key.save` (malformed params, not a validation outcome).
fn validate_save(req: &KeySaveRequest) -> std::result::Result<(), String> {
    validate_name(&req.name)?;
    let bad = |m: &str| m.is_empty() || m.len() > 200 || m.chars().any(char::is_control);
    if bad(req.model.trim()) {
        return Err("model must be a model name of at most 200 characters".into());
    }
    if let Some(m) = light_model(req) {
        if bad(&m) {
            return Err("light_model must be a model name of at most 200 characters".into());
        }
        validate_name(&light_name(&req.name))
            .map_err(|_| "with light_model, name must be at most 26 characters".to_owned())?;
    }
    Ok(())
}

/// Whether `v` can be an environment variable name: a letter or `_`, then letters, digits or
/// `_`, at most 128.
fn valid_env_name(v: &str) -> bool {
    let mut b = v.bytes();
    v.len() <= 128
        && b.next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// The usual key variables of providers (as their own tools name them).
const PROVIDER_ENV: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENROUTER_API_KEY",
    "GROQ_API_KEY",
    "MISTRAL_API_KEY",
    "DEEPSEEK_API_KEY",
    "TOGETHER_API_KEY",
    "GEMINI_API_KEY",
    "XAI_API_KEY",
];

/// Nugget's own secrets in `NUGGET_*_KEY` variables, never usable as a provider key.
const INTERNAL_ENV: &[&str] = &[
    "NUGGET_MANIFEST_SIGNING_KEY",
    "NUGGET_PLUGIN_SIGNING_KEY",
    "NUGGET_TRUST_DEV_MANIFEST_KEY",
];

/// Whether `key_ref: "env:VAR"` may read `var`: `NUGGET_…_KEY` (not one of Nugget's own
/// secrets, nor the runtime's `NUGGET_RUNTIME_…` keys) or a provider's usual variable.
///
/// Compared in upper case: Windows looks variables up ignoring case, so
/// `NUGGET_Plugin_Signing_KEY` there reads `NUGGET_PLUGIN_SIGNING_KEY` and must be refused like it.
fn env_allowed(var: &str) -> bool {
    let var = var.to_ascii_uppercase();
    let var = var.as_str();
    PROVIDER_ENV.contains(&var)
        || (var.starts_with("NUGGET_")
            && var.ends_with("_KEY")
            && var.len() > "NUGGET__KEY".len()
            && !var.starts_with("NUGGET_RUNTIME_")
            && !INTERNAL_ENV.contains(&var))
}

/// Where the key comes from.
#[derive(Debug, Clone)]
pub(crate) enum KeySpec {
    /// Typed into the app; goes to the secret store.
    Typed(ApiKey),
    /// The daemon's environment variable of that name.
    Env(String),
    /// No key: a server on this computer ([`keyless_allowed`]).
    None,
}

/// What config.toml refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeyRef {
    /// `keychain:NAME`.
    Keychain(String),
    /// `env:VAR`.
    Env(String),
}

impl KeyRef {
    fn kind(&self) -> &'static str {
        match self {
            KeyRef::Keychain(_) => "keychain",
            KeyRef::Env(_) => "env",
        }
    }

    fn as_config(&self) -> String {
        match self {
            KeyRef::Keychain(n) => format!("keychain:{n}"),
            KeyRef::Env(v) => format!("env:{v}"),
        }
    }
}

impl KeySpec {
    /// The key itself, or why it cannot be read.
    ///
    /// Only variables meant for AI keys ([`env_allowed`], or `also`: the variable a provider
    /// already uses) are read, so a caller cannot send another secret of the daemon's
    /// environment to a server of its choice. A variable that is not allowed, unset or empty
    /// gets the same answer, so callers cannot probe which variables exist.
    fn read(
        &self,
        env: &dyn Fn(&str) -> Option<String>,
        also: Option<&str>,
    ) -> std::result::Result<Option<String>, KeyTestResult> {
        match self {
            KeySpec::None => Ok(None),
            KeySpec::Typed(k) => Ok(Some(k.0.clone())),
            KeySpec::Env(var) => match (env_allowed(var) || also == Some(var.as_str()))
                .then(|| env(var))
                .flatten()
            {
                Some(v) if !v.trim().is_empty() => Ok(Some(v)),
                _ => Err(KeyTestResult::fail(
                    Reason::EnvMissing,
                    format!(
                        "Nugget can't use the environment variable {var}. Use a variable whose name starts with NUGGET_ and ends with _KEY (or a provider's usual one, such as OPENAI_API_KEY), set it where Nugget starts, then restart Nugget."
                    ),
                )),
            },
        }
    }

    fn stored_ref(&self, name: &str) -> Option<KeyRef> {
        match self {
            KeySpec::Typed(_) => Some(KeyRef::Keychain(name.to_owned())),
            KeySpec::Env(v) => Some(KeyRef::Env(v.clone())),
            KeySpec::None => None,
        }
    }
}

/// `spec`, unless it is [`KeySpec::None`] for anything but an OpenAI-compatible server on a
/// loopback address (local servers such as Ollama and LM Studio need no key).
fn keyless_allowed(
    spec: KeySpec,
    kind: Kind,
    base: Option<&str>,
) -> std::result::Result<KeySpec, String> {
    let local = kind == Kind::OpenaiCompatible
        && base
            .and_then(scheme_and_host)
            .is_some_and(|(_, h)| is_loopback(h));
    match spec {
        KeySpec::None if !local => Err("api_key or key_ref is required".into()),
        s => Ok(s),
    }
}

/// Exactly one of `api_key` and `key_ref` (`env:VAR`); malformed params otherwise.
fn key_spec(
    api_key: Option<&ApiKey>,
    key_ref: Option<&str>,
) -> std::result::Result<KeySpec, String> {
    match (api_key, key_ref.map(str::trim).filter(|r| !r.is_empty())) {
        (Some(k), None) => Ok(KeySpec::Typed(k.clone())),
        (None, Some(r)) => match r.strip_prefix("env:").map(str::trim) {
            Some(v) if valid_env_name(v) => Ok(KeySpec::Env(v.to_owned())),
            Some(_) => Err(
                "key_ref: the variable name may only have letters, digits and \"_\", and must not start with a digit"
                    .into(),
            ),
            None => Err("key_ref must be \"env:VARIABLE_NAME\"".into()),
        },
        (Some(_), Some(_)) => Err("give api_key or key_ref, not both".into()),
        (None, None) => Ok(KeySpec::None),
    }
}

/// A base URL as given, trimmed, without a trailing slash; `None` when empty.
fn clean_base(base: Option<&str>) -> Option<String> {
    base.map(|b| b.trim().trim_end_matches('/').to_owned())
        .filter(|b| !b.is_empty())
}

/// Whether `host` (from a URL, IPv6 possibly in brackets) is this machine.
fn is_loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Split `scheme://host[:port]/...` into scheme and host; `None` if it is not that shape.
fn scheme_and_host(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = if authority.starts_with('[') {
        &authority[..=authority.find(']')?]
    } else {
        authority.split(':').next().unwrap_or("")
    };
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some((scheme, host))
}

/// The URL to ask for the model list, or why the address cannot be used.
fn models_url(kind: Kind, base: Option<&str>) -> std::result::Result<String, KeyTestResult> {
    let base = match (clean_base(base), kind) {
        (Some(b), _) => b,
        (None, Kind::OpenaiResponses) => OPENAI_DEFAULT_BASE.to_owned(),
        (None, Kind::Anthropic) => ANTHROPIC_DEFAULT_BASE.to_owned(),
        (None, Kind::OpenaiCompatible) => {
            return Err(KeyTestResult::fail(
                Reason::WrongUrl,
                "Enter the provider's address (it usually ends in /v1).",
            ));
        }
    };
    let Some((scheme, host)) = scheme_and_host(&base) else {
        return Err(KeyTestResult::fail(
            Reason::WrongUrl,
            "That address is not a valid web address. It should look like https://example.com/v1.",
        ));
    };
    match scheme.to_ascii_lowercase().as_str() {
        "https" => {}
        "http" if is_loopback(host) => {}
        "http" => {
            return Err(KeyTestResult::fail(
                Reason::WrongUrl,
                "Use an https:// address. Plain http:// is only allowed for a server on this computer.",
            ));
        }
        _ => {
            return Err(KeyTestResult::fail(
                Reason::WrongUrl,
                "The address must start with https://.",
            ));
        }
    }
    Ok(match kind {
        Kind::Anthropic => format!("{base}/v1/models"),
        Kind::OpenaiCompatible | Kind::OpenaiResponses => format!("{base}/models"),
    })
}

/// One key check.
#[derive(Clone)]
pub(crate) struct Check {
    pub kind: Kind,
    pub base_url: Option<String>,
    /// `None`: a keyless local server (no auth header).
    pub key: Option<String>,
    pub model: Option<String>,
}

impl std::fmt::Debug for Check {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Check")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// Ask the provider for its models with the key and classify the answer. Never fails: every
/// outcome is a [`KeyTestResult`].
pub(crate) async fn check_key(http: &MeteredHttp, req: &Check, timeout: Duration) -> KeyTestResult {
    let key = req.key.as_deref().map(str::trim);
    if key.is_some_and(str::is_empty) {
        return KeyTestResult::fail(Reason::BadKey, "Enter an API key.");
    }
    if key.is_some_and(|k| !k.bytes().all(|b| b.is_ascii_graphic())) {
        return KeyTestResult::fail(
            Reason::BadKey,
            "The key contains spaces or characters that API keys never have. Copy it again.",
        );
    }
    let url = match models_url(req.kind, req.base_url.as_deref()) {
        Ok(u) => u,
        Err(r) => return r,
    };
    // A provider whose model list is public: check the key where it is checked first.
    if let (Some(k), Some(key_url)) = (key, public_list_key_url(&url))
        && let Err(r) = authed_get(http, &key_url, req.kind, Some(k), timeout).await
    {
        return r;
    }
    let resp = match authed_get(http, &url, req.kind, key, timeout).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let Some(models) = model_ids(&resp.body) else {
        return KeyTestResult::fail(
            Reason::WrongUrl,
            "Nothing that looks like an AI provider answered at this address. Check the URL (it usually ends in /v1).",
        );
    };
    let mut message = if key.is_some() {
        format!("Key works. Found {} models.", models.len())
    } else {
        format!("The server answered. Found {} models.", models.len())
    };
    if let Some(m) = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        && !models.is_empty()
        && !models.iter().any(|id| id == m)
    {
        message.push_str(" The model you entered is not in the provider's list; check its name.");
    }
    KeyTestResult {
        ok: true,
        reason: Reason::Ok,
        message,
        models,
    }
}

/// GET `url` with the key as `kind` sends it; the response if it is a 2xx, else the classified
/// failure.
async fn authed_get(
    http: &MeteredHttp,
    url: &str,
    kind: Kind,
    key: Option<&str>,
    timeout: Duration,
) -> std::result::Result<nugget_connector::http::Response, KeyTestResult> {
    let get = "GET".parse().expect("GET is a method");
    let builder = http.request(get, url).header("accept", "application/json");
    let builder = match (kind, key) {
        (_, None) => builder,
        (Kind::Anthropic, Some(key)) => builder
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION),
        (Kind::OpenaiCompatible | Kind::OpenaiResponses, Some(key)) => {
            builder.header("authorization", format!("Bearer {key}"))
        }
    };
    let resp = match tokio::time::timeout(timeout, http.send(builder, &[])).await {
        Err(_) => {
            return Err(KeyTestResult::fail(
                Reason::NoNetwork,
                "The provider did not answer in time. Check your internet connection or the address.",
            ));
        }
        Ok(Err(Error::Network(_))) => {
            return Err(KeyTestResult::fail(
                Reason::NoNetwork,
                "Could not reach the provider. Check your internet connection or the address.",
            ));
        }
        Ok(Err(Error::Invalid(e))) if e.starts_with("bad request") => {
            return Err(KeyTestResult::fail(
                Reason::WrongUrl,
                "That address is not a valid web address. It should look like https://example.com/v1.",
            ));
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "key check failed");
            return Err(KeyTestResult::fail(
                Reason::ServerError,
                "The provider sent an answer Nugget could not read. Try again later.",
            ));
        }
        Ok(Ok(r)) => r,
    };
    let status = resp.status.as_u16();
    tracing::debug!(status, kind = kind.as_str(), "key check answered");
    match status {
        200..=299 => {}
        401 | 403 => {
            return Err(KeyTestResult::fail(
                Reason::BadKey,
                "The provider rejected this key. Check that you copied all of it.",
            ));
        }
        // Gemini answers a bad key with 400 "API key not valid".
        400 if String::from_utf8_lossy(&resp.body).contains("API key") => {
            return Err(KeyTestResult::fail(
                Reason::BadKey,
                "The provider rejected this key. Check that you copied all of it.",
            ));
        }
        300..=399 | 404 | 405 => {
            return Err(KeyTestResult::fail(
                Reason::WrongUrl,
                "Nothing that looks like an AI provider answered at this address. Check the URL (it usually ends in /v1).",
            ));
        }
        429 => {
            return Err(KeyTestResult::fail(
                Reason::ServerError,
                "The provider is busy or the account is out of quota. Try again in a minute.",
            ));
        }
        _ => {
            return Err(KeyTestResult::fail(
                Reason::ServerError,
                format!("The provider had a problem (HTTP {status}). Try again later."),
            ));
        }
    }
    Ok(resp)
}

/// For a provider whose model list answers without a key, the URL that checks the key:
/// OpenRouter's `GET /api/v1/key`.
fn public_list_key_url(models_url: &str) -> Option<String> {
    let (_, host) = scheme_and_host(models_url)?;
    host.eq_ignore_ascii_case("openrouter.ai")
        .then(|| {
            models_url
                .strip_suffix("/models")
                .map(|b| format!("{b}/key"))
        })
        .flatten()
}

/// Model ids from an OpenAI- or Anthropic-style list (`{"data": [{"id": ...}, ...]}`), or a
/// bare array of such objects (Together).
fn model_ids(body: &[u8]) -> Option<Vec<String>> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let data = v.get("data").unwrap_or(&v).as_array()?;
    Some(
        data.iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str))
            // Gemini may list `models/NAME`; requests take `NAME`.
            .map(|id| id.strip_prefix("models/").unwrap_or(id))
            .filter(|id| !id.is_empty() && id.len() <= 200)
            .take(MAX_MODELS)
            .map(str::to_owned)
            .collect(),
    )
}

/// The provider to add to `config.toml`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NewProvider {
    pub name: String,
    pub kind: Kind,
    pub base_url: Option<String>,
    pub model: String,
    /// The `light` tier's own model: a `NAME-light` table with the same key and address.
    pub light_model: Option<String>,
    /// `None`: a server on this computer that needs no key.
    pub key: Option<KeyRef>,
    /// A loopback server that forwards prompts to the cloud: `locality = "cloud"`.
    pub cloud_forwarder: bool,
}

impl NewProvider {
    /// One `[[providers]]` table (values escaped by the TOML writer).
    fn table(&self, id: String, model: &str, tiers: &[&str]) -> toml::Value {
        let mut t = toml::Table::new();
        t.insert("id".into(), id.into());
        t.insert("kind".into(), self.kind.as_str().into());
        if let Some(b) = &self.base_url {
            t.insert("base_url".into(), b.clone().into());
        }
        t.insert("model".into(), model.into());
        if let Some(k) = &self.key {
            t.insert("api_key".into(), k.as_config().into());
        }
        if self.cloud_forwarder {
            t.insert("locality".into(), "cloud".into());
        }
        let tiers = tiers.iter().copied().map(toml::Value::from).collect();
        t.insert("tiers".into(), toml::Value::Array(tiers));
        toml::Value::Table(t)
    }

    /// The `[[providers]]` table(s), serialized. A keyless on-device server also triages (local
    /// only); a light model moves the `light` tier to its own table.
    fn toml(&self) -> Result<String> {
        let on_device = self.key.is_none() && !self.cloud_forwarder;
        let mut main: Vec<&str> = Vec::new();
        if on_device {
            main.push("triage");
        }
        if self.light_model.is_none() {
            main.push("light");
        }
        main.extend(["synthesis", "agentic"]);
        let mut tables = vec![self.table(self.name.clone(), &self.model, &main)];
        if let Some(m) = &self.light_model {
            tables.push(self.table(light_name(&self.name), m, &["light"]));
        }
        let mut doc = toml::Table::new();
        doc.insert("providers".into(), toml::Value::Array(tables));
        toml::to_string(&doc).map_err(|e| Error::Other(format!("provider table: {e}")))
    }

    fn comment(&self) -> String {
        match &self.key {
            Some(KeyRef::Keychain(_)) => {
                format!("{MARKER} The key is in the OS keychain, not in this file.\n")
            }
            Some(KeyRef::Env(v)) => format!(
                "{MARKER} The key is read from the environment variable {v}, not stored in this file.\n"
            ),
            None => format!("{MARKER} A server on this computer; it needs no key.\n"),
        }
    }
}

/// `config.toml` at `path`, or `None` when there is none.
fn read_config(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The providers in `config.toml` at `path` (none when there is no file).
fn read_providers(path: &Path) -> Result<Vec<ProviderConfig>> {
    let Some(text) = read_config(path)? else {
        return Ok(Vec::new());
    };
    Ok(Config::parse(&text)
        .map_err(|e| Error::Invalid(format!("invalid config {}: {e}", path.display())))?
        .providers)
}

/// Provider `name` in `config.toml` at `path`.
fn find_provider(path: &Path, name: &str) -> Result<Option<ProviderConfig>> {
    Ok(read_providers(path)?
        .into_iter()
        .find(|p| p.id.as_str() == name))
}

/// Whether `config.toml` at `path` already has a provider `name` (a missing file has none).
fn provider_exists(path: &Path, name: &str) -> Result<bool> {
    let Some(text) = read_config(path)? else {
        return Ok(false);
    };
    let table: toml::Table = toml::from_str(&text)
        .map_err(|e| Error::Invalid(format!("invalid config {}: {e}", path.display())))?;
    Ok(table
        .get("providers")
        .and_then(toml::Value::as_array)
        .is_some_and(|ps| {
            ps.iter()
                .any(|p| p.get("id").and_then(toml::Value::as_str) == Some(name))
        }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Written {
    Saved,
    Exists,
    /// The secret store refused the key; nothing was written.
    NoKeychain,
}

/// Write `text` to `path` whole (temporary file, then rename).
fn write_config(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    // Keep the file's permissions (for example 0600) on the replacement.
    let perms = std::fs::metadata(path).ok().map(|m| m.permissions());
    std::fs::write(&tmp, text)
        .and_then(|()| match perms {
            Some(p) => std::fs::set_permissions(&tmp, p),
            None => Ok(()),
        })
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
    Ok(())
}

/// Store `key` in `secrets` under `account`; `false` (logged, without the key) when the store
/// cannot hold it.
fn store_key(secrets: &dyn SecretStore, account: &str, key: &str) -> bool {
    match secrets.set(account, key) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "the secret store cannot hold the provider key");
            false
        }
    }
}

/// Store the key (for a `keychain:` entry) under [`provider_account`]`(name)` and append the
/// provider to `config.toml`. The new file is validated as a [`Config`] before anything is
/// stored. A missing config starts from [`DEFAULT_CONFIG`].
pub(crate) fn write_provider(
    path: &Path,
    secrets: &dyn SecretStore,
    entry: &NewProvider,
    key: Option<&str>,
) -> Result<Written> {
    if provider_exists(path, &entry.name)?
        || (entry.light_model.is_some() && provider_exists(path, &light_name(&entry.name))?)
    {
        return Ok(Written::Exists);
    }
    let mut text = read_config(path)?.unwrap_or_else(|| DEFAULT_CONFIG.to_owned());
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&entry.comment());
    text.push_str(&entry.toml()?);
    Config::parse(&text)
        .map_err(|e| Error::Invalid(format!("the new provider is invalid: {e}")))?;

    let account = match (&entry.key, key) {
        (Some(KeyRef::Keychain(n)), Some(key)) => {
            let account = provider_account(n);
            if !store_key(secrets, &account, key) {
                return Ok(Written::NoKeychain);
            }
            Some(account)
        }
        (Some(KeyRef::Keychain(_)), None) => {
            return Err(Error::Invalid("a keychain provider needs a key".into()));
        }
        _ => None,
    };
    if let Err(e) = write_config(path, &text) {
        if let Some(a) = &account {
            let _ = secrets.delete(a);
        }
        return Err(e);
    }
    Ok(Written::Saved)
}

/// What `ai.key.update` changes.
#[derive(Debug, Clone)]
pub(crate) struct KeyChange {
    /// What config.toml refers to now.
    pub old: Option<ApiKeyRef>,
    /// What it should refer to.
    pub new: KeyRef,
    /// A new address (`Some(None)` removes it).
    pub base_url: Option<Option<String>>,
}

/// Replace provider `name`'s key: overwrite the stored secret, and rewrite its `api_key` /
/// `base_url` lines in `config.toml` when they change (nothing else in the file changes).
pub(crate) fn replace_key(
    path: &Path,
    secrets: &dyn SecretStore,
    name: &str,
    change: &KeyChange,
    key: String,
) -> Result<Written> {
    let text = read_config(path)?.ok_or_else(|| Error::NotFound("config.toml".into()))?;
    let old_ref = change.old.as_ref().map(ToString::to_string);
    let new_ref = change.new.as_config();
    let mut edits: Vec<(&str, Option<String>)> = Vec::new();
    if old_ref.as_deref() != Some(new_ref.as_str()) {
        edits.push(("api_key", Some(new_ref.clone())));
    }
    if let Some(b) = &change.base_url {
        edits.push(("base_url", b.clone()));
    }
    let new_text = if edits.is_empty() {
        None
    } else {
        let mut t = edit_provider_text(&text, name, &edits)
            .ok_or_else(|| Error::Invalid(format!("cannot find provider {name} in config.toml")))?;
        if let Some(light) = companion_id(&text, name) {
            t = edit_provider_text(&t, &light, &edits)
                .ok_or_else(|| Error::Invalid(format!("cannot edit provider {light}")))?;
        }
        Some(t)
    };

    // The keychain entry to write (the one config.toml will refer to).
    let account = match &change.new {
        KeyRef::Keychain(n) => Some(provider_account(n)),
        KeyRef::Env(_) => None,
    };
    let previous = match &account {
        Some(a) => secrets.get(a).ok().flatten(),
        None => None,
    };
    if let Some(a) = &account
        && !store_key(secrets, a, &key)
    {
        return Ok(Written::NoKeychain);
    }
    if let Some(t) = &new_text
        && let Err(e) = write_config(path, t)
    {
        if let Some(a) = &account {
            let _ = match &previous {
                Some(p) => secrets.set(a, p),
                None => secrets.delete(a),
            };
        }
        return Err(e);
    }
    // A keychain entry no provider refers to any more is deleted.
    if let Some(ApiKeyRef::Keychain(old)) = &change.old
        && change.new != KeyRef::Keychain(old.clone())
        && !keychain_refs(new_text.as_deref().unwrap_or(&text)).contains(old)
        && let Err(e) = secrets.delete(&provider_account(old))
    {
        tracing::warn!(error = %e, "cannot delete the old provider key");
    }
    Ok(Written::Saved)
}

/// The id of provider `name`'s `light` companion in config `text`, if it has one.
fn companion_id(text: &str, name: &str) -> Option<String> {
    let all = Config::parse(text).ok()?.providers;
    let light = light_name(name);
    all.iter()
        .find(|c| c.id.as_str() == light && companion_of(&all, c).is_some())
        .map(|c| c.id.to_string())
}

/// Delete provider `name`'s stored key (if it has one in the keychain) and remove its table (and
/// its `light` companion's) from `config.toml`. Returns the ids removed.
pub(crate) fn remove_provider(
    path: &Path,
    secrets: &dyn SecretStore,
    name: &str,
    key: Option<&ApiKeyRef>,
) -> Result<Vec<String>> {
    let text = read_config(path)?.ok_or_else(|| Error::NotFound("config.toml".into()))?;
    let mut removed = vec![name.to_owned()];
    let mut new_text = remove_provider_text(&text, name)
        .ok_or_else(|| Error::Invalid(format!("cannot find provider {name} in config.toml")))?;
    if let Some(light) = companion_id(&text, name) {
        new_text = remove_provider_text(&new_text, &light)
            .ok_or_else(|| Error::Invalid(format!("cannot remove provider {light}")))?;
        removed.push(light);
    }
    write_config(path, &new_text)?;
    // Another provider may share the entry; then it stays.
    if let Some(ApiKeyRef::Keychain(n)) = key
        && !keychain_refs(&new_text).contains(n)
        && let Err(e) = secrets.delete(&provider_account(n))
    {
        tracing::warn!(error = %e, "cannot delete the provider key from the secret store");
    }
    Ok(removed)
}

/// The `keychain:NAME` names the providers in config `text` refer to.
fn keychain_refs(text: &str) -> Vec<String> {
    Config::parse(text)
        .map(|c| {
            c.providers
                .into_iter()
                .filter_map(|p| match p.api_key {
                    Some(ApiKeyRef::Keychain(n)) => Some(n),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One `[[providers]]` table in the text of a config file, with its sub-tables
/// (`[providers.headers]`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    /// Start of the header line.
    start: usize,
    /// End of the table's last line that is neither blank nor a comment (comments right before
    /// the next table belong to that table).
    end: usize,
    /// End of the provider's own keys (before its first sub-table).
    keys_end: usize,
    id: Option<String>,
}

/// What a table header line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Header {
    /// `[[providers]]`.
    Provider,
    /// `[providers.x]` or `[[providers.x]]`: part of the provider above.
    Sub,
    Other,
}

/// The part of `line` before a comment (`#` outside a string). A line that opens or closes a
/// multi-line string is returned whole.
fn code_part(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if quote.is_none() && (line[i..].starts_with("\"\"\"") || line[i..].starts_with("'''")) {
            return line;
        }
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => return &line[..i],
            None => {}
        }
    }
    line
}

/// The kind of table header `line` is, if it is one.
fn header(line: &str) -> Option<Header> {
    let t = code_part(line).trim();
    if !t.starts_with('[') {
        return None;
    }
    let compact: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    Some(if compact == "[[providers]]" {
        Header::Provider
    } else if compact.starts_with("[providers.") || compact.starts_with("[[providers.") {
        Header::Sub
    } else {
        Header::Other
    })
}

/// The `[[providers]]` tables of `text`, in order.
fn provider_spans(text: &str) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut open: Option<Span> = None;
    let mut in_multiline = false;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let (start, end) = (pos, pos + line.len());
        pos = end;
        let was_multiline = in_multiline;
        // Inside a multi-line string everything counts; outside, a comment does not.
        let counted = if was_multiline { line } else { code_part(line) };
        let toggles = counted.matches("\"\"\"").count() + counted.matches("'''").count();
        if toggles % 2 == 1 {
            in_multiline = !in_multiline;
        }
        if !was_multiline && let Some(h) = header(line) {
            if let (Header::Sub, Some(s)) = (h, open.as_mut()) {
                s.end = end;
                continue;
            }
            if let Some(s) = open.take() {
                spans.push(s);
            }
            if h == Header::Provider {
                open = Some(Span {
                    start,
                    end,
                    keys_end: end,
                    id: None,
                });
            }
            continue;
        }
        if let Some(s) = open.as_mut() {
            let t = line.trim();
            if was_multiline || !(t.is_empty() || t.starts_with('#')) {
                if s.keys_end == s.end {
                    s.keys_end = end;
                }
                s.end = end;
            }
        }
    }
    if let Some(s) = open {
        spans.push(s);
    }
    for s in &mut spans {
        s.id = toml::from_str::<toml::Table>(&text[s.start..s.end])
            .ok()
            .and_then(|t| {
                t.get("providers")?
                    .as_array()?
                    .first()?
                    .get("id")?
                    .as_str()
                    .map(str::to_owned)
            });
    }
    spans
}

/// The parsed document of `text` with provider `name` removed (`edits: None`) or with those keys
/// of it changed; an empty `providers` array counts as none.
fn expected_doc(
    text: &str,
    name: &str,
    edits: Option<&[(&str, Option<String>)]>,
) -> Option<toml::Table> {
    let mut doc: toml::Table = toml::from_str(text).ok()?;
    let ps = doc.get_mut("providers")?.as_array_mut()?;
    let i = ps
        .iter()
        .position(|p| p.get("id").and_then(toml::Value::as_str) == Some(name))?;
    match edits {
        None => {
            ps.remove(i);
        }
        Some(edits) => {
            let t = ps[i].as_table_mut()?;
            for (k, v) in edits {
                match v {
                    Some(v) => t.insert((*k).to_owned(), toml::Value::String(v.clone())),
                    None => t.remove(*k),
                };
            }
        }
    }
    Some(normalized(doc))
}

fn normalized(mut doc: toml::Table) -> toml::Table {
    if doc
        .get("providers")
        .and_then(toml::Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        doc.remove("providers");
    }
    doc
}

/// Whether `out` is a valid config whose parsed document is exactly `want`.
fn same_doc(out: &str, want: &toml::Table) -> bool {
    Config::parse(out).is_ok()
        && toml::from_str::<toml::Table>(out).is_ok_and(|d| &normalized(d) == want)
}

/// `text` without provider `name`'s table (with its sub-tables, and the comment `ai.key.save`
/// wrote above it). Every other byte is kept. `None` if the table cannot be found or the result
/// would not be exactly the same document minus that provider.
fn remove_provider_text(text: &str, name: &str) -> Option<String> {
    let span = provider_spans(text)
        .into_iter()
        .find(|s| s.id.as_deref() == Some(name))?;
    let mut start = span.start;
    // Our own comment line directly above, and the blank line `ai.key.save` put before it.
    let line_before = |at: usize| -> Option<usize> {
        let prev = text[..at].strip_suffix('\n')?;
        Some(prev.rfind('\n').map_or(0, |i| i + 1))
    };
    if let Some(ls) = line_before(start)
        && text[ls..start].trim_start().starts_with(MARKER)
    {
        start = ls;
        if let Some(ls) = line_before(start)
            && text[ls..start].trim().is_empty()
        {
            start = ls;
        }
    }
    let out = format!("{}{}", &text[..start], &text[span.end..]);
    same_doc(&out, &expected_doc(text, name, None)?).then_some(out)
}

/// `text` with the given keys of provider `name`'s table set (a line `key = "value"` replaced
/// in place or added after the header) or removed (`None`). Every other line is kept. `None` if
/// the table cannot be found or the result is not exactly the same document with those changes.
fn edit_provider_text(text: &str, name: &str, edits: &[(&str, Option<String>)]) -> Option<String> {
    let span = provider_spans(text)
        .into_iter()
        .find(|s| s.id.as_deref() == Some(name))?;
    // Only the provider's own keys, not its sub-tables'.
    let body = &text[span.start..span.keys_end];
    let mut lines: Vec<String> = body.split_inclusive('\n').map(str::to_owned).collect();
    if lines.last().is_some_and(|l| !l.ends_with('\n')) {
        lines.last_mut()?.push('\n');
    }
    let key_of = |line: &str| -> Option<String> {
        let (k, _) = line.split_once('=')?;
        Some(k.trim().trim_matches('"').to_owned())
    };
    for (key, value) in edits {
        let new_line = value
            .as_ref()
            .map(|v| format!("{key} = {}\n", toml::Value::String(v.clone())));
        let at = lines
            .iter()
            .skip(1)
            .position(|l| key_of(l).as_deref() == Some(*key))
            .map(|i| i + 1);
        match (at, new_line) {
            (Some(i), Some(l)) => {
                let indent: String = lines[i].chars().take_while(|c| c.is_whitespace()).collect();
                lines[i] = format!("{indent}{l}");
            }
            (Some(i), None) => {
                lines.remove(i);
            }
            (None, Some(l)) => lines.insert(1, l),
            (None, None) => {}
        }
    }
    let mut new_body = lines.concat();
    if !body.ends_with('\n') {
        new_body.pop();
    }
    let out = format!(
        "{}{}{}",
        &text[..span.start],
        new_body,
        &text[span.keys_end..]
    );
    same_doc(&out, &expected_doc(text, name, Some(edits))?).then_some(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use nugget_core::egress::EgressEvent;
    use nugget_ingest::MemorySecrets;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    const GOOD: &str = "sk-test-good";

    #[derive(Default)]
    struct Ledger(Mutex<Vec<EgressEvent>>);
    impl EgressSink for Ledger {
        fn record(&self, event: EgressEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    /// A minimal OpenAI-compatible server: `GET /v1/models` with `Bearer sk-test-good` lists two
    /// models; another key is 401; any other path is 404; `/broken/v1/models` is 500 and
    /// `/html/v1/models` is a 200 web page.
    async fn mock() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).to_lowercase();
                    let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                    let authed = head.contains(&format!("authorization: bearer {GOOD}"));
                    let (status, ctype, body) = match path.as_str() {
                        "/v1/models" if authed => (
                            "200 OK",
                            "application/json",
                            r#"{"object":"list","data":[{"id":"model-a"},{"id":"model-b"}]}"#,
                        ),
                        "/v1/models" => (
                            "401 Unauthorized",
                            "application/json",
                            r#"{"error":"bad key"}"#,
                        ),
                        "/broken/v1/models" => ("500 Internal Server Error", "text/plain", "oops"),
                        "/html/v1/models" => ("200 OK", "text/html", "<html>hello</html>"),
                        _ => ("404 Not Found", "text/plain", "not found"),
                    };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn req(kind: Kind, base: Option<&str>, key: &str) -> Check {
        Check {
            kind,
            base_url: base.map(str::to_owned),
            key: Some(key.to_owned()),
            model: None,
        }
    }

    fn client() -> (MeteredHttp, Arc<Ledger>) {
        let ledger = Arc::new(Ledger::default());
        (metered(ledger.clone()).unwrap(), ledger)
    }

    const T: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn classifies_failures_without_echoing_the_key() {
        let base = mock().await;
        let (http, ledger) = client();
        let cases = [
            (
                req(
                    Kind::OpenaiCompatible,
                    Some(&format!("{base}/v1")),
                    "sk-wrong",
                ),
                Reason::BadKey,
            ),
            (
                req(Kind::OpenaiCompatible, Some(&format!("{base}/nope")), GOOD),
                Reason::WrongUrl,
            ),
            (
                req(
                    Kind::OpenaiCompatible,
                    Some(&format!("{base}/html/v1")),
                    GOOD,
                ),
                Reason::WrongUrl,
            ),
            (
                req(
                    Kind::OpenaiCompatible,
                    Some(&format!("{base}/broken/v1")),
                    GOOD,
                ),
                Reason::ServerError,
            ),
            (req(Kind::OpenaiCompatible, None, GOOD), Reason::WrongUrl),
            (
                req(Kind::OpenaiCompatible, Some("http://example.com/v1"), GOOD),
                Reason::WrongUrl,
            ),
            (
                req(Kind::OpenaiCompatible, Some("ftp://localhost/v1"), GOOD),
                Reason::WrongUrl,
            ),
            (
                req(Kind::OpenaiCompatible, Some("not a url"), GOOD),
                Reason::WrongUrl,
            ),
            (
                req(Kind::OpenaiCompatible, Some(&format!("{base}/v1")), ""),
                Reason::BadKey,
            ),
            (
                req(
                    Kind::OpenaiCompatible,
                    Some(&format!("{base}/v1")),
                    "sk bad",
                ),
                Reason::BadKey,
            ),
        ];
        for (r, want) in cases {
            let got = check_key(&http, &r, T).await;
            assert!(!got.ok);
            assert_eq!(got.reason, want, "{:?}: {got:?}", r.base_url);
            assert!(!got.message.contains("sk-"), "{}", got.message);
        }
        // Only the requests that went out are recorded (4 to the mock server).
        assert_eq!(ledger.0.lock().unwrap().len(), 4);
    }

    #[test]
    fn urls_and_loopback() {
        assert_eq!(
            models_url(Kind::OpenaiResponses, None).unwrap(),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            models_url(Kind::Anthropic, None).unwrap(),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(
            models_url(Kind::Anthropic, Some(" https://gw.example/ ")).unwrap(),
            "https://gw.example/v1/models"
        );
        assert!(models_url(Kind::OpenaiCompatible, Some("http://localhost:1234/v1")).is_ok());
        assert!(models_url(Kind::OpenaiCompatible, Some("http://[::1]:8080/v1")).is_ok());
        assert!(models_url(Kind::OpenaiCompatible, Some("http://10.0.0.2/v1")).is_err());
        assert!(models_url(Kind::OpenaiCompatible, Some("https://u:p@host/v1")).is_err());
        assert!(is_loopback("127.0.0.5") && is_loopback("LOCALHOST") && !is_loopback("10.0.0.1"));
    }

    #[test]
    fn key_refs_are_env_variables_with_valid_names() {
        let k = ApiKey(GOOD.into());
        assert!(matches!(key_spec(Some(&k), None), Ok(KeySpec::Typed(_))));
        assert!(matches!(
            key_spec(None, Some(" env: NUGGET_OPENAI_KEY ")),
            Ok(KeySpec::Env(v)) if v == "NUGGET_OPENAI_KEY"
        ));
        assert!(matches!(
            key_spec(None, Some("env:_x1")),
            Ok(KeySpec::Env(_))
        ));
        for bad in [
            "env:",
            "env:1ABC",
            "env:A-B",
            "env:A B",
            "keychain:x",
            "NAME",
        ] {
            assert!(key_spec(None, Some(bad)).is_err(), "{bad}");
        }
        assert!(key_spec(Some(&k), Some("env:A")).is_err(), "both");
        // Neither: no key, which only a local server may have (`keyless_allowed`).
        assert!(matches!(key_spec(None, None), Ok(KeySpec::None)), "neither");
        assert!(
            matches!(key_spec(None, Some("  ")), Ok(KeySpec::None)),
            "blank is neither"
        );
        assert!(!valid_env_name(&"A".repeat(129)));

        // Reading: an allowed, set variable is the key. Unset, empty or not allowed (any other
        // secret of the daemon's environment) all get the same answer, so nothing can be probed.
        let env = |v: &str| match v {
            "NUGGET_SET_KEY"
            | "OPENAI_API_KEY"
            | "AWS_SECRET_ACCESS_KEY"
            | "GITHUB_TOKEN"
            | "NUGGET_MCP_TOKEN"
            | "NUGGET_PLUGIN_SIGNING_KEY"
            | "NUGGET_RUNTIME_API_KEY_CHAT"
            | "WORK_KEY" => Some(GOOD.to_owned()),
            "NUGGET_EMPTY_KEY" => Some("  ".to_owned()),
            _ => None,
        };
        for v in ["NUGGET_SET_KEY", "OPENAI_API_KEY"] {
            assert_eq!(
                KeySpec::Env(v.into()).read(&env, None).unwrap().as_deref(),
                Some(GOOD),
                "{v}"
            );
        }
        let generic = |v: &str| {
            KeySpec::Env(v.into())
                .read(&env, None)
                .unwrap_err()
                .message
                .replace(v, "VAR")
        };
        let want = generic("NUGGET_UNSET_KEY");
        for v in [
            "NUGGET_EMPTY_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "NUGGET_MCP_TOKEN",
            "NUGGET_PLUGIN_SIGNING_KEY",
            "NUGGET_RUNTIME_API_KEY_CHAT",
            "NUGGET__KEY",
            "WORK_KEY",
        ] {
            let r = KeySpec::Env(v.into()).read(&env, None).unwrap_err();
            assert_eq!(r.reason, Reason::EnvMissing, "{v}");
            assert_eq!(generic(v), want, "{v}");
            assert!(!r.message.contains(GOOD));
        }
        // Windows looks variables up ignoring case: another spelling of a refused variable
        // must not read it.
        let windows = |v: &str| env(&v.to_ascii_uppercase());
        for v in [
            "NUGGET_Plugin_Signing_KEY",
            "NUGGET_Manifest_Signing_KEY",
            "NUGGET_Trust_Dev_Manifest_KEY",
        ] {
            let r = KeySpec::Env(v.into()).read(&windows, None).unwrap_err();
            assert_eq!(r.reason, Reason::EnvMissing, "{v}");
            assert!(!r.message.contains(GOOD));
        }
        assert_eq!(
            KeySpec::Env("Openai_Api_Key".into())
                .read(&windows, None)
                .unwrap()
                .as_deref(),
            Some(GOOD)
        );
        // The variable a provider already uses stays usable for it.
        assert_eq!(
            KeySpec::Env("WORK_KEY".into())
                .read(&env, Some("WORK_KEY"))
                .unwrap()
                .as_deref(),
            Some(GOOD)
        );
        assert_eq!(
            KeySpec::Env("V".into()).stored_ref("custom"),
            Some(KeyRef::Env("V".into()))
        );
        assert_eq!(
            KeySpec::Typed(k).stored_ref("custom"),
            Some(KeyRef::Keychain("custom".into()))
        );
        assert_eq!(suggested_env("my-openai"), "NUGGET_MY_OPENAI_KEY");
    }

    fn entry(name: &str, key: KeyRef) -> NewProvider {
        NewProvider {
            name: name.into(),
            kind: Kind::OpenaiCompatible,
            base_url: Some("http://127.0.0.1:9/v1".into()),
            model: "model \"a\"".into(),
            light_model: None,
            key: Some(key),
            cloud_forwarder: false,
        }
    }

    const HAND_WRITTEN: &str = r#"# My config. Keep this comment.
[runtime]
enabled = false   # trailing comment

# The local server.
[[providers]]
id = "ollama"
kind = "openai_compatible"
base_url = "http://localhost:11434/v1"
model = "llama"
tiers = ["light"]

# Work key, set by hand.
[[providers]]
id = "work"
kind = "anthropic"
model = "claude-x"
api_key = "env:WORK_KEY"   # in my shell profile
tiers = ["synthesis"]
"#;

    #[test]
    fn replace_and_remove_keep_every_other_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, HAND_WRITTEN).unwrap();
        let secrets = MemorySecrets::new();
        let e = entry("custom", KeyRef::Keychain("custom".into()));
        write_provider(&path, &secrets, &e, Some(GOOD)).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with(HAND_WRITTEN));

        // Same keychain reference: only the secret changes.
        let change = KeyChange {
            old: Some(ApiKeyRef::Keychain("custom".into())),
            new: KeyRef::Keychain("custom".into()),
            base_url: None,
        };
        assert_eq!(
            replace_key(&path, &secrets, "custom", &change, "sk-new".into()).unwrap(),
            Written::Saved
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        assert_eq!(
            secrets.get(&provider_account("custom")).unwrap().as_deref(),
            Some("sk-new")
        );

        // A hand-written env: provider moves to the keychain, with a new address; only its
        // api_key line changes (comment and all) and base_url is added after the header.
        let change = KeyChange {
            old: Some(ApiKeyRef::Env("WORK_KEY".into())),
            new: KeyRef::Keychain("work".into()),
            base_url: Some(Some("https://gw.example".into())),
        };
        replace_key(&path, &secrets, "work", &change, "sk-work".into()).unwrap();
        let edited = std::fs::read_to_string(&path).unwrap();
        let want = saved
            .replace(
                "api_key = \"env:WORK_KEY\"   # in my shell profile\n",
                "api_key = \"keychain:work\"\n",
            )
            .replace(
                "[[providers]]\nid = \"work\"\n",
                "[[providers]]\nbase_url = \"https://gw.example\"\nid = \"work\"\n",
            );
        assert_eq!(edited, want);
        assert_eq!(
            secrets.get(&provider_account("work")).unwrap().as_deref(),
            Some("sk-work")
        );

        // Removing ours drops the table and our comment; the rest is byte for byte the same.
        remove_provider(
            &path,
            &secrets,
            "custom",
            Some(&ApiKeyRef::Keychain("custom".into())),
        )
        .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after,
            want.replace(&format!("\n{}{}", e.comment(), e.toml().unwrap()), "")
        );
        assert!(secrets.get(&provider_account("custom")).unwrap().is_none());

        // Removing a hand-written one in the middle keeps the comment that belongs to the next
        // table and everything else.
        let text = HAND_WRITTEN.replace(
            "tiers = [\"light\"]\n\n# Work key",
            "tiers = [\"light\"]\napi_key = \"env:OLLAMA\"\n\n# Work key",
        );
        std::fs::write(&path, &text).unwrap();
        remove_provider(&path, &secrets, "ollama", None).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after,
            text.replace(
                "[[providers]]\nid = \"ollama\"\nkind = \"openai_compatible\"\nbase_url = \"http://localhost:11434/v1\"\nmodel = \"llama\"\ntiers = [\"light\"]\napi_key = \"env:OLLAMA\"\n",
                ""
            )
        );
        assert!(after.contains("# The local server.\n\n# Work key"));
        assert!(remove_provider_text(&after, "nope").is_none());
    }

    #[test]
    fn a_keychain_entry_another_provider_uses_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let text = r#"[[providers]]
id = "one"
kind = "openai_compatible"
base_url = "http://localhost:1/v1"
model = "m"
api_key = "keychain:shared"

[[providers]]
id = "two"
kind = "openai_compatible"
base_url = "http://localhost:2/v1"
model = "m"
api_key = "keychain:shared"
"#;
        std::fs::write(&path, text).unwrap();
        let secrets = MemorySecrets::new();
        secrets.set(&provider_account("shared"), GOOD).unwrap();
        // "one" moves to its own entry: "shared" stays for "two".
        let change = KeyChange {
            old: Some(ApiKeyRef::Keychain("shared".into())),
            new: KeyRef::Keychain("one".into()),
            base_url: None,
        };
        replace_key(&path, &secrets, "one", &change, "sk-one".into()).unwrap();
        assert_eq!(
            secrets.get(&provider_account("shared")).unwrap().as_deref(),
            Some(GOOD)
        );
        // Removing "two" (the last user) deletes it.
        let shared = ApiKeyRef::Keychain("shared".into());
        remove_provider(&path, &secrets, "two", Some(&shared)).unwrap();
        assert!(secrets.get(&provider_account("shared")).unwrap().is_none());
        assert_eq!(
            secrets.get(&provider_account("one")).unwrap().as_deref(),
            Some("sk-one")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rewriting_the_config_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, DEFAULT_CONFIG).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let e = entry("custom", KeyRef::Keychain("custom".into()));
        write_provider(&path, &MemorySecrets::new(), &e, Some(GOOD)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// A wiremock server answering `GET path` with `status` and `body`.
    async fn answer(path: &str, status: u16, body: Value) -> wiremock::MockServer {
        use wiremock::matchers::{method, path as p};
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("GET"))
            .and(p(path))
            .respond_with(wiremock::ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn detects_local_servers_without_a_key_and_flags_cloud_forwarders() {
        let ollama = answer(
            "/v1/models",
            200,
            json!({"data": [{"id": "qwen3:4b"}, {"id": "gpt-oss:120b-cloud"}]}),
        )
        .await;
        let base = format!("{}/v1", ollama.uri());
        let (http, ledger) = client();
        let servers = [
            ("ollama", "Ollama", base.as_str()),
            ("lmstudio", "LM Studio", "http://127.0.0.1:9/v1"),
        ];
        let found = detect_local(&http, &servers, &[format!("{base}/")], T).await;
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].id, "ollama");
        assert_eq!(found[0].models, ["qwen3:4b"]);
        assert_eq!(found[0].cloud_models, ["gpt-oss:120b-cloud"]);
        assert!(found[0].configured);
        // No key was sent, and the request is in the ledger.
        let reqs = ollama.received_requests().await.unwrap();
        assert!(
            reqs.iter()
                .all(|r| !r.headers.contains_key("authorization"))
        );
        assert!(!ledger.0.lock().unwrap().is_empty());
        // A key-less check is for loopback OpenAI-compatible servers only.
        let none = || KeySpec::None;
        assert!(keyless_allowed(none(), Kind::OpenaiCompatible, Some(&base)).is_ok());
        assert!(
            keyless_allowed(none(), Kind::OpenaiCompatible, Some("https://api.x.ai/v1")).is_err()
        );
        assert!(keyless_allowed(none(), Kind::Anthropic, Some(&base)).is_err());
        assert!(keyless_allowed(none(), Kind::OpenaiResponses, None).is_err());
    }

    #[tokio::test]
    async fn gemini_bad_key_and_model_prefix() {
        let bad = answer(
            "/v1beta/openai/models",
            400,
            json!({"error": {"code": 400, "message": "API key not valid. Please pass a valid API key.", "status": "INVALID_ARGUMENT"}}),
        )
        .await;
        let (http, _) = client();
        let base = format!("{}/v1beta/openai", bad.uri());
        let r = check_key(
            &http,
            &req(Kind::OpenaiCompatible, Some(&base), "AIza-wrong"),
            T,
        )
        .await;
        assert_eq!(r.reason, Reason::BadKey, "{r:?}");
        let good = answer(
            "/v1beta/openai/models",
            200,
            json!({"object": "list", "data": [{"id": "models/gemini-x-flash"}]}),
        )
        .await;
        let base = format!("{}/v1beta/openai", good.uri());
        let r = check_key(&http, &req(Kind::OpenaiCompatible, Some(&base), GOOD), T).await;
        assert_eq!(r.models, ["gemini-x-flash"], "{r:?}");
        assert_eq!(
            model_ids(br#"[{"id": "a/b"}]"#),
            Some(vec!["a/b".to_owned()])
        );
        assert_eq!(model_ids(br#"{"error": "x"}"#), None);
    }

    #[test]
    fn openrouter_key_is_checked_where_its_list_is_not_public() {
        assert_eq!(
            public_list_key_url("https://openrouter.ai/api/v1/models").as_deref(),
            Some("https://openrouter.ai/api/v1/key")
        );
        assert_eq!(
            public_list_key_url("https://api.groq.com/openai/v1/models"),
            None
        );
    }

    #[test]
    fn light_model_pair_is_written_replaced_and_removed_together() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, HAND_WRITTEN).unwrap();
        let secrets = MemorySecrets::new();
        let mut e = entry("cloud", KeyRef::Env("NUGGET_A_KEY".into()));
        e.base_url = Some("https://api.example.com/v1".into());
        e.light_model = Some("small".into());
        assert_eq!(
            write_provider(&path, &secrets, &e, None).unwrap(),
            Written::Saved
        );
        let all = read_providers(&path).unwrap();
        let tiers = |id: &str| {
            all.iter()
                .find(|c| c.id.as_str() == id)
                .unwrap()
                .tiers
                .clone()
        };
        assert_eq!(tiers("cloud"), [Tier::Synthesis, Tier::Agentic]);
        assert_eq!(tiers("cloud-light"), [Tier::Light]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(companion_id(&text, "cloud").as_deref(), Some("cloud-light"));
        // A taken companion name refuses the save.
        assert_eq!(
            write_provider(&path, &secrets, &e, None).unwrap(),
            Written::Exists
        );

        let change = KeyChange {
            old: Some(ApiKeyRef::Env("NUGGET_A_KEY".into())),
            new: KeyRef::Env("NUGGET_B_KEY".into()),
            base_url: None,
        };
        replace_key(&path, &secrets, "cloud", &change, GOOD.into()).unwrap();
        let all = read_providers(&path).unwrap();
        for id in ["cloud", "cloud-light"] {
            let c = all.iter().find(|c| c.id.as_str() == id).unwrap();
            assert_eq!(
                c.api_key,
                Some(ApiKeyRef::Env("NUGGET_B_KEY".into())),
                "{id}"
            );
        }
        let key = Some(ApiKeyRef::Env("NUGGET_B_KEY".into()));
        let removed = remove_provider(&path, &secrets, "cloud", key.as_ref()).unwrap();
        assert_eq!(removed, ["cloud", "cloud-light"]);
        let left = std::fs::read_to_string(&path).unwrap();
        assert_eq!(left.trim_end(), HAND_WRITTEN.trim_end());
    }

    #[test]
    fn keyless_local_and_cloud_forwarding_tables() {
        let local = |model: &str| {
            let base = Some("http://127.0.0.1:11434/v1".to_owned());
            NewProvider {
                name: "ollama".into(),
                kind: Kind::OpenaiCompatible,
                cloud_forwarder: is_cloud_forwarder(base.as_deref(), model),
                base_url: base,
                model: model.into(),
                light_model: None,
                key: None,
            }
        };
        let parse = |p: &NewProvider| {
            Config::parse(&p.toml().unwrap())
                .unwrap()
                .providers
                .remove(0)
        };
        let on_device = parse(&local("qwen3:4b"));
        assert_eq!(on_device.api_key, None);
        assert_eq!(on_device.locality, None);
        assert!(on_device.tiers.contains(&Tier::Triage));
        // Looks local, but sends prompts to the cloud: never triage, marked cloud.
        let forwarder = parse(&local("gpt-oss:120b-cloud"));
        assert_eq!(forwarder.locality, Some(Locality::Cloud));
        assert!(!forwarder.tiers.contains(&Tier::Triage));
        assert!(!is_cloud_forwarder(
            Some("https://api.example.com/v1"),
            "x-cloud"
        ));
    }
}
