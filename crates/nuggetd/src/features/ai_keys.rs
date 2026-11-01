//! "Bring your own key": check a cloud (or self-hosted) provider key, save it, replace it,
//! remove it, and route to it right away.
//!
//! - `ai.key.test` asks the provider for its model list (`GET {base}/models` with a bearer token
//!   for the OpenAI kinds, `GET {base}/v1/models` with `x-api-key` for Anthropic) and classifies
//!   the answer: `ok`, `bad_key`, `wrong_url`, `no_network` or `server_error`. The outcome is a
//!   result, never a JSON-RPC error (only malformed params are).
//! - `ai.key.save` runs the same check, then stores the key with the daemon's [`SecretStore`]
//!   under [`provider_account`] and appends a `[[providers]]` table to `config.toml` that refers
//!   to it as `keychain:NAME` (the key itself never reaches the file). A provider with the same
//!   id is refused (`exists`; the app then offers `ai.key.update`).
//! - Instead of `api_key`, `key_ref: "env:VAR"` names an environment variable of the daemon that
//!   holds the key (the fallback when there is no keychain: `ai.key.save` answers `no_keychain`
//!   if the secret store cannot hold the key). The variable is read to check the key, and
//!   `config.toml` gets `api_key = "env:VAR"`.
//! - `ai.key.update` checks a new key (or variable, or address) for a provider that refers to a
//!   key (`keychain:` or `env:`) and replaces it; `ai.key.remove` deletes the stored key and
//!   removes the provider's table from `config.toml`, leaving every other byte of the file as it
//!   was. `ai.key.saved` lists those providers.
//! - Saved, updated and removed providers take effect at once: the provider is built (its key read
//!   from the same secret store) and added to, swapped in or dropped from the router
//!   ([`State::set_provider`]); `restart_required` stays in results for compatibility and is
//!   always `false`.
//!
//! Every request is recorded in the egress ledger under feature `ai_setup` (also for loopback
//! servers: the user asked for the check and should see it). The key is never logged, echoed in
//! messages or written anywhere but the secret store. Only `https` is accepted, except for
//! loopback hosts (`localhost`, `127.0.0.1`, `::1`).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nugget_connector::SecretStore;
use nugget_connector::http::MeteredHttp;
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
const METHODS: &[&str] = &[
    AI_KEY_TEST,
    AI_KEY_SAVE,
    AI_KEY_UPDATE,
    AI_KEY_REMOVE,
    AI_KEY_SAVED,
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
                    .map_err(RpcError::invalid_params)?;
                let result = self.save(req, spec).await?;
                to_result(result)
            }
            AI_KEY_UPDATE => {
                let req: KeyUpdateRequest = parse_params(params)?;
                validate_name(&req.name).map_err(RpcError::invalid_params)?;
                let spec = key_spec(req.api_key.as_ref(), req.key_ref.as_deref())
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
        // Refuse a taken id before going to the network (checked again when writing).
        if provider_exists(&config_path, &req.name)? {
            return Ok(exists_result(&req.name));
        }
        if reserved(&req.name, |id| {
            self.state.router.provider(&ProviderId::new(id)).is_some()
                || self.state.slot(&ProviderId::new(id)).is_some()
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
        let check = Check {
            kind: req.kind,
            base_url: req.base_url.clone(),
            key: key.clone(),
            model: Some(req.model.clone()),
        };
        let checked = check_key(&self.http, &check, CHECK_TIMEOUT).await;
        self.flush_egress().await;
        if !checked.ok {
            return Ok(KeySaveResult::from_test(checked));
        }
        let _guard = self.write.lock().await;
        let secrets = self.state.secrets.clone();
        let entry = NewProvider {
            name: req.name.clone(),
            kind: req.kind,
            base_url: clean_base(req.base_url.as_deref()),
            model: req.model.trim().to_owned(),
            key: spec.stored_ref(&req.name),
        };
        let written = {
            let entry = entry.clone();
            let path = config_path.clone();
            blocking(move || write_provider(&path, &*secrets, &entry, key.trim())).await?
        };
        match written {
            Written::Saved => {}
            Written::Exists => return Ok(exists_result(&req.name)),
            Written::NoKeychain => return Ok(no_keychain_result(&req.name)),
        }
        tracing::info!(provider = %entry.name, kind = entry.kind.as_str(), "provider key saved");
        self.audit(
            "ai.key.saved",
            &entry.name,
            json!({
                "kind": entry.kind.as_str(),
                "base_url": entry.base_url,
                "model": entry.model,
                "key": entry.key.kind(),
            }),
        )
        .await;
        self.route(&entry.name).await;
        Ok(KeySaveResult {
            ok: true,
            reason: Reason::Ok,
            message: "Key checked and saved. You can use it now.".into(),
            models: checked.models,
            saved: true,
            provider_id: Some(entry.name),
            restart_required: false,
            suggested_env: None,
        })
    }

    async fn update(&self, req: KeyUpdateRequest, spec: KeySpec) -> Result<KeySaveResult> {
        let config_path = self.state.paths.config_file();
        let _guard = self.write.lock().await;
        let current = {
            let path = config_path.clone();
            let name = req.name.clone();
            blocking(move || find_provider(&path, &name)).await?
        };
        let cfg = match editable(current.as_ref(), &req.name) {
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
            Ok(k) => k,
            Err(r) => return Ok(KeySaveResult::from_test(r)),
        };
        let base_url = match req.base_url.as_deref() {
            Some(b) => clean_base(Some(b)),
            None => cfg.base_url.clone(),
        };
        let kind = Kind::of(cfg.kind);
        let check = Check {
            kind,
            base_url: base_url.clone(),
            key: key.clone(),
            model: Some(cfg.model.clone()),
        };
        let checked = check_key(&self.http, &check, CHECK_TIMEOUT).await;
        self.flush_egress().await;
        if !checked.ok {
            return Ok(KeySaveResult::from_test(checked));
        }
        let secrets = self.state.secrets.clone();
        let (path, name) = (config_path.clone(), req.name.clone());
        let new_ref = spec.stored_ref(&req.name);
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
        tracing::info!(provider = %req.name, kind = kind.as_str(), "provider key replaced");
        self.audit(
            "ai.key.updated",
            &req.name,
            json!({
                "kind": kind.as_str(),
                "base_url": base_url,
                "model": cfg.model,
                "key": new_ref.kind(),
            }),
        )
        .await;
        self.route(&req.name).await;
        Ok(KeySaveResult {
            ok: true,
            reason: Reason::Ok,
            message: "Key checked and replaced. Nugget uses the new key now.".into(),
            models: checked.models,
            saved: true,
            provider_id: Some(req.name),
            restart_required: false,
            suggested_env: None,
        })
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
        blocking(move || remove_provider(&path, &*secrets, &owned, key.as_ref())).await?;
        self.state.remove_provider(&ProviderId::new(name));
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
            message: format!("\"{name}\" was removed and its key deleted."),
            removed: true,
            provider_id: name.to_owned(),
        })
    }

    async fn saved(&self) -> Result<SavedList> {
        let path = self.state.paths.config_file();
        let configs = blocking(move || read_providers(&path)).await?;
        let providers = configs
            .into_iter()
            .filter(|c| editable(Some(c), c.id.as_str()).is_ok())
            .map(|c| {
                let (key, env_var) = match &c.api_key {
                    Some(ApiKeyRef::Env(v)) => ("env", Some(v.clone())),
                    _ => ("keychain", None),
                };
                let error = self.state.unavailable_reason(&c.id);
                let available = error.is_none() && self.state.router.provider(&c.id).is_some();
                SavedProvider {
                    id: c.id.to_string(),
                    kind: Kind::of(c.kind),
                    base_url: c.base_url.clone(),
                    model: c.model.clone(),
                    key: key.into(),
                    env_var,
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
/// a key and is not one of the runtime's own (`local-*`).
fn editable<'a>(
    cfg: Option<&'a ProviderConfig>,
    name: &str,
) -> std::result::Result<&'a ProviderConfig, Reason> {
    let cfg = cfg.ok_or(Reason::NotFound)?;
    if name.starts_with("local-") || cfg.api_key.is_none() {
        return Err(Reason::NotEditable);
    }
    Ok(cfg)
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
    let m = req.model.trim();
    if m.is_empty() || m.len() > 200 || m.chars().any(char::is_control) {
        return Err("model must be a model name of at most 200 characters".into());
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
fn env_allowed(var: &str) -> bool {
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
    ) -> std::result::Result<String, KeyTestResult> {
        match self {
            KeySpec::Typed(k) => Ok(k.0.clone()),
            KeySpec::Env(var) => match (env_allowed(var) || also == Some(var.as_str()))
                .then(|| env(var))
                .flatten()
            {
                Some(v) if !v.trim().is_empty() => Ok(v),
                _ => Err(KeyTestResult::fail(
                    Reason::EnvMissing,
                    format!(
                        "Nugget can't use the environment variable {var}. Use a variable whose name starts with NUGGET_ and ends with _KEY (or a provider's usual one, such as OPENAI_API_KEY), set it where Nugget starts, then restart Nugget."
                    ),
                )),
            },
        }
    }

    fn stored_ref(&self, name: &str) -> KeyRef {
        match self {
            KeySpec::Typed(_) => KeyRef::Keychain(name.to_owned()),
            KeySpec::Env(v) => KeyRef::Env(v.clone()),
        }
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
        (None, None) => Err("api_key or key_ref is required".into()),
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
    pub key: String,
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
    let key = req.key.trim();
    if key.is_empty() {
        return KeyTestResult::fail(Reason::BadKey, "Enter an API key.");
    }
    if !key.bytes().all(|b| b.is_ascii_graphic()) {
        return KeyTestResult::fail(
            Reason::BadKey,
            "The key contains spaces or characters that API keys never have. Copy it again.",
        );
    }
    let url = match models_url(req.kind, req.base_url.as_deref()) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let get = "GET".parse().expect("GET is a method");
    let builder = http.request(get, &url).header("accept", "application/json");
    let builder = match req.kind {
        Kind::Anthropic => builder
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION),
        Kind::OpenaiCompatible | Kind::OpenaiResponses => {
            builder.header("authorization", format!("Bearer {key}"))
        }
    };
    let resp = match tokio::time::timeout(timeout, http.send(builder, &[])).await {
        Err(_) => {
            return KeyTestResult::fail(
                Reason::NoNetwork,
                "The provider did not answer in time. Check your internet connection or the address.",
            );
        }
        Ok(Err(Error::Network(_))) => {
            return KeyTestResult::fail(
                Reason::NoNetwork,
                "Could not reach the provider. Check your internet connection or the address.",
            );
        }
        Ok(Err(Error::Invalid(e))) if e.starts_with("bad request") => {
            return KeyTestResult::fail(
                Reason::WrongUrl,
                "That address is not a valid web address. It should look like https://example.com/v1.",
            );
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "key check failed");
            return KeyTestResult::fail(
                Reason::ServerError,
                "The provider sent an answer Nugget could not read. Try again later.",
            );
        }
        Ok(Ok(r)) => r,
    };
    let status = resp.status.as_u16();
    tracing::debug!(status, kind = req.kind.as_str(), "key check answered");
    match status {
        200..=299 => {}
        401 | 403 => {
            return KeyTestResult::fail(
                Reason::BadKey,
                "The provider rejected this key. Check that you copied all of it.",
            );
        }
        300..=399 | 404 | 405 => {
            return KeyTestResult::fail(
                Reason::WrongUrl,
                "Nothing that looks like an AI provider answered at this address. Check the URL (it usually ends in /v1).",
            );
        }
        429 => {
            return KeyTestResult::fail(
                Reason::ServerError,
                "The provider is busy or the account is out of quota. Try again in a minute.",
            );
        }
        _ => {
            return KeyTestResult::fail(
                Reason::ServerError,
                format!("The provider had a problem (HTTP {status}). Try again later."),
            );
        }
    }
    let Some(models) = model_ids(&resp.body) else {
        return KeyTestResult::fail(
            Reason::WrongUrl,
            "Nothing that looks like an AI provider answered at this address. Check the URL (it usually ends in /v1).",
        );
    };
    let mut message = format!("Key works. Found {} models.", models.len());
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

/// Model ids from an OpenAI- or Anthropic-style list (`{"data": [{"id": ...}, ...]}`).
fn model_ids(body: &[u8]) -> Option<Vec<String>> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let data = v.get("data")?.as_array()?;
    Some(
        data.iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str))
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
    pub key: KeyRef,
}

impl NewProvider {
    /// The `[[providers]]` table, serialized (values escaped by the TOML writer).
    fn toml(&self) -> Result<String> {
        let mut t = toml::Table::new();
        t.insert("id".into(), self.name.clone().into());
        t.insert("kind".into(), self.kind.as_str().into());
        if let Some(b) = &self.base_url {
            t.insert("base_url".into(), b.clone().into());
        }
        t.insert("model".into(), self.model.clone().into());
        t.insert("api_key".into(), self.key.as_config().into());
        t.insert(
            "tiers".into(),
            toml::Value::Array(
                ["light", "synthesis", "agentic"]
                    .into_iter()
                    .map(toml::Value::from)
                    .collect(),
            ),
        );
        let mut doc = toml::Table::new();
        doc.insert(
            "providers".into(),
            toml::Value::Array(vec![toml::Value::Table(t)]),
        );
        toml::to_string(&doc).map_err(|e| Error::Other(format!("provider table: {e}")))
    }

    fn comment(&self) -> String {
        match &self.key {
            KeyRef::Keychain(_) => {
                format!("{MARKER} The key is in the OS keychain, not in this file.\n")
            }
            KeyRef::Env(v) => format!(
                "{MARKER} The key is read from the environment variable {v}, not stored in this file.\n"
            ),
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
    key: &str,
) -> Result<Written> {
    if provider_exists(path, &entry.name)? {
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

    let account = match &entry.key {
        KeyRef::Keychain(n) => {
            let account = provider_account(n);
            if !store_key(secrets, &account, key) {
                return Ok(Written::NoKeychain);
            }
            Some(account)
        }
        KeyRef::Env(_) => None,
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
        let t = edit_provider_text(&text, name, &edits)
            .ok_or_else(|| Error::Invalid(format!("cannot find provider {name} in config.toml")))?;
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

/// Delete provider `name`'s stored key (if it has one in the keychain) and remove its table from
/// `config.toml`.
pub(crate) fn remove_provider(
    path: &Path,
    secrets: &dyn SecretStore,
    name: &str,
    key: Option<&ApiKeyRef>,
) -> Result<()> {
    let text = read_config(path)?.ok_or_else(|| Error::NotFound("config.toml".into()))?;
    let new_text = remove_provider_text(&text, name)
        .ok_or_else(|| Error::Invalid(format!("cannot find provider {name} in config.toml")))?;
    write_config(path, &new_text)?;
    // Another provider may share the entry; then it stays.
    if let Some(ApiKeyRef::Keychain(n)) = key
        && !keychain_refs(&new_text).contains(n)
        && let Err(e) = secrets.delete(&provider_account(n))
    {
        tracing::warn!(error = %e, "cannot delete the provider key from the secret store");
    }
    Ok(())
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

    /// A secret store like Linux without a Secret Service: every call fails.
    struct NoKeyring;
    impl SecretStore for NoKeyring {
        fn get(&self, _: &str) -> Result<Option<String>> {
            Err(Error::Storage(
                "keychain: Platform secure storage failure: no secret service provider or dbus session found".into(),
            ))
        }
        fn set(&self, a: &str, _: &str) -> Result<()> {
            self.get(a).map(|_| ())
        }
        fn delete(&self, a: &str) -> Result<()> {
            self.get(a).map(|_| ())
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
            key: key.to_owned(),
            model: None,
        }
    }

    fn client() -> (MeteredHttp, Arc<Ledger>) {
        let ledger = Arc::new(Ledger::default());
        (metered(ledger.clone()).unwrap(), ledger)
    }

    const T: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn good_key_lists_models_and_is_in_the_ledger() {
        let base = mock().await;
        let (http, ledger) = client();
        let r = check_key(
            &http,
            &req(Kind::OpenaiCompatible, Some(&format!("{base}/v1/")), GOOD),
            T,
        )
        .await;
        assert!(r.ok, "{r:?}");
        assert_eq!(r.reason, Reason::Ok);
        assert_eq!(r.models, ["model-a", "model-b"]);
        assert_eq!(r.message, "Key works. Found 2 models.");
        let events = ledger.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].feature, "ai_setup");
        assert!(base.ends_with(&events[0].host), "{}", events[0].host);
        assert!(events[0].bytes_sent > 0 && events[0].bytes_received > 0);
    }

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

    #[tokio::test]
    async fn closed_port_is_no_network() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let (http, ledger) = client();
        let r = check_key(
            &http,
            &req(
                Kind::OpenaiCompatible,
                Some(&format!("http://127.0.0.1:{port}/v1")),
                GOOD,
            ),
            T,
        )
        .await;
        assert_eq!(r.reason, Reason::NoNetwork, "{r:?}");
        assert_eq!(ledger.0.lock().unwrap().len(), 1, "attempt recorded");
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
    fn save_checks_names() {
        let mut r = KeySaveRequest {
            name: "my-openai_2".into(),
            kind: Kind::OpenaiResponses,
            base_url: None,
            api_key: Some(ApiKey(GOOD.into())),
            key_ref: None,
            model: "gpt-x".into(),
        };
        assert!(validate_save(&r).is_ok());
        assert!(!format!("{r:?}").contains(GOOD));
        for bad in ["", "Upper", "has space", "x".repeat(33).as_str(), "a.b"] {
            r.name = bad.into();
            assert!(validate_save(&r).is_err(), "{bad:?}");
        }
        r.name = "ok".into();
        r.model = "  ".into();
        assert!(validate_save(&r).is_err());
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
        assert!(key_spec(None, None).is_err(), "neither");
        assert!(key_spec(None, Some("  ")).is_err(), "blank is neither");
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
                KeySpec::Env(v.into()).read(&env, None).unwrap(),
                GOOD,
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
        // The variable a provider already uses stays usable for it.
        assert_eq!(
            KeySpec::Env("WORK_KEY".into())
                .read(&env, Some("WORK_KEY"))
                .unwrap(),
            GOOD
        );
        assert_eq!(
            KeySpec::Env("V".into()).stored_ref("custom"),
            KeyRef::Env("V".into())
        );
        assert_eq!(
            KeySpec::Typed(k).stored_ref("custom"),
            KeyRef::Keychain("custom".into())
        );
        assert_eq!(suggested_env("my-openai"), "NUGGET_MY_OPENAI_KEY");
    }

    fn entry(name: &str, key: KeyRef) -> NewProvider {
        NewProvider {
            name: name.into(),
            kind: Kind::OpenaiCompatible,
            base_url: Some("http://127.0.0.1:9/v1".into()),
            model: "model \"a\"".into(),
            key,
        }
    }

    #[test]
    fn write_provider_refers_to_the_keychain_and_refuses_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, DEFAULT_CONFIG).unwrap();
        let secrets = MemorySecrets::new();
        let entry = entry("custom", KeyRef::Keychain("custom".into()));
        assert_eq!(
            write_provider(&path, &secrets, &entry, GOOD).unwrap(),
            Written::Saved
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(GOOD), "plaintext key in config");
        assert!(text.contains("api_key = \"keychain:custom\""), "{text}");
        assert!(text.starts_with(DEFAULT_CONFIG), "existing text kept");
        let cfg = Config::parse(&text).unwrap();
        let p = cfg
            .providers
            .iter()
            .find(|p| p.id.as_str() == "custom")
            .unwrap();
        assert_eq!(p.model, "model \"a\"");
        assert_eq!(p.base_url.as_deref(), Some("http://127.0.0.1:9/v1"));
        assert_eq!(p.tiers.len(), 3);
        assert_eq!(
            cfg.providers.len(),
            1,
            "the default config has none of its own"
        );
        assert_eq!(
            secrets.get(&provider_account("custom")).unwrap().as_deref(),
            Some(GOOD)
        );

        // The same id again is refused, and nothing changes.
        let other = MemorySecrets::new();
        assert_eq!(
            write_provider(&path, &other, &entry, "sk-other").unwrap(),
            Written::Exists
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(other.get(&provider_account("custom")).unwrap().is_none());
    }

    #[test]
    fn write_provider_starts_from_the_default_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cfg").join("config.toml");
        let secrets = MemorySecrets::new();
        let entry = NewProvider {
            name: "openai".into(),
            kind: Kind::OpenaiResponses,
            base_url: None,
            model: "gpt-x".into(),
            key: KeyRef::Keychain("openai".into()),
        };
        write_provider(&path, &secrets, &entry, GOOD).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("base_url = \"https://api.openai"));
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.providers.len(), 1);
    }

    #[test]
    fn no_keyring_writes_nothing_and_env_refs_need_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, DEFAULT_CONFIG).unwrap();

        // Keychain refused: config untouched, no raw error.
        let e = entry("custom", KeyRef::Keychain("custom".into()));
        assert_eq!(
            write_provider(&path, &NoKeyring, &e, GOOD).unwrap(),
            Written::NoKeychain
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);
        let r = no_keychain_result("custom");
        assert_eq!(r.reason, Reason::NoKeychain);
        assert!(!r.ok && !r.saved);
        assert_eq!(r.suggested_env.as_deref(), Some("NUGGET_CUSTOM_KEY"));
        assert!(r.message.contains("environment variable"), "{}", r.message);
        assert!(!r.message.contains("dbus"), "{}", r.message);

        // An env: reference is written without touching the secret store at all.
        let e = entry("custom", KeyRef::Env("NUGGET_CUSTOM_KEY".into()));
        assert_eq!(
            write_provider(&path, &NoKeyring, &e, GOOD).unwrap(),
            Written::Saved
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("api_key = \"env:NUGGET_CUSTOM_KEY\""),
            "{text}"
        );
        assert!(
            text.contains("environment variable NUGGET_CUSTOM_KEY"),
            "{text}"
        );
        assert!(!text.contains(GOOD));

        // Replacing with a typed key still needs the keyring; removing works without one.
        let change = KeyChange {
            old: Some(ApiKeyRef::Env("NUGGET_CUSTOM_KEY".into())),
            new: KeyRef::Keychain("custom".into()),
            base_url: None,
        };
        assert_eq!(
            replace_key(&path, &NoKeyring, "custom", &change, GOOD.into()).unwrap(),
            Written::NoKeychain
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        remove_provider(
            &path,
            &NoKeyring,
            "custom",
            Some(&ApiKeyRef::Env("NUGGET_CUSTOM_KEY".into())),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);
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
        write_provider(&path, &secrets, &e, GOOD).unwrap();
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
    fn comments_and_sub_tables_do_not_confuse_the_spans() {
        let text = r#"[runtime]
enabled = false

[[providers]]
id = "a"
kind = "openai_compatible"
base_url = "http://localhost:1/v1"
model = "m"
api_key = "env:NUGGET_A_KEY"
tiers = ["light"]
# a note with """ in it
[providers.headers]
api_key = "env:NUGGET_HDR_KEY"

[draft]
note = """
[[providers]]
"""

[[providers]]
id = "b"
kind = "openai_compatible"
base_url = "http://localhost:2/v1"
model = "m"
api_key = "keychain:a"   # '''
"#;
        Config::parse(text).unwrap();
        // Removing "a" takes its headers sub-table and leaves [draft] (and its multi-line
        // string that looks like a table) alone.
        let out = remove_provider_text(text, "a").unwrap();
        assert!(
            out.contains("[draft]\nnote = \"\"\"\n[[providers]]\n\"\"\"\n"),
            "{out}"
        );
        assert!(!out.contains("[providers.headers]"), "{out}");
        let doc: toml::Table = toml::from_str(&out).unwrap();
        assert_eq!(doc["providers"].as_array().unwrap().len(), 1);
        assert!(doc.contains_key("draft"));
        // Editing "a" changes its own api_key, not the header of that name.
        let out = edit_provider_text(text, "a", &[("api_key", Some("keychain:a".into()))]).unwrap();
        assert!(out.contains("api_key = \"keychain:a\"\ntiers"), "{out}");
        assert!(out.contains("api_key = \"env:NUGGET_HDR_KEY\""), "{out}");
        assert!(remove_provider_text(&out, "b").is_some());

        // A comment with """ right before a table no longer swallows the table after it.
        let tricky = text.replace(
            "[providers.headers]\napi_key = \"env:NUGGET_HDR_KEY\"\n",
            "",
        );
        let out = remove_provider_text(&tricky, "a").unwrap();
        assert_eq!(
            toml::from_str::<toml::Table>(&out).unwrap()["draft"],
            toml::from_str::<toml::Table>(&tricky).unwrap()["draft"]
        );
        // The comment right before the next table belongs to it and stays.
        assert!(
            out.starts_with("[runtime]\nenabled = false\n\n# a note with \"\"\" in it\n\n[draft]"),
            "{out}"
        );
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
        write_provider(&path, &MemorySecrets::new(), &e, GOOD).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn nuggets_own_names_are_reserved() {
        assert!(reserved("local-chat", |_| false));
        assert!(reserved("local-anything", |_| false));
        assert!(reserved("slot", |id| id == "slot"));
        assert!(!reserved("custom", |id| id == "slot"));
    }

    #[test]
    fn only_key_backed_providers_are_editable() {
        let cfg = Config::parse(HAND_WRITTEN).unwrap();
        let find = |id: &str| cfg.providers.iter().find(|p| p.id.as_str() == id);
        assert_eq!(
            editable(find("nope"), "nope").unwrap_err(),
            Reason::NotFound
        );
        assert_eq!(
            editable(find("ollama"), "ollama").unwrap_err(),
            Reason::NotEditable
        );
        assert!(editable(find("work"), "work").is_ok());
        let mut local = find("work").unwrap().clone();
        local.id = ProviderId::new("local-chat");
        assert_eq!(
            editable(Some(&local), "local-chat").unwrap_err(),
            Reason::NotEditable
        );
    }

    #[test]
    fn the_store_resolver_reads_the_secret_store_and_environment() {
        let secrets = Arc::new(MemorySecrets::new());
        secrets
            .set(&provider_account("custom"), " sk-x \n")
            .unwrap();
        let r = StoreResolver {
            secrets: secrets.clone(),
            env: Arc::new(|v| (v == "SET").then(|| "sk-env".to_owned())),
        };
        let get = |s: &str| r.resolve(&s.parse().unwrap());
        assert_eq!(get("keychain:custom").unwrap().expose(), "sk-x");
        assert_eq!(get("env:SET").unwrap().expose(), "sk-env");
        assert!(matches!(get("env:UNSET"), Err(Error::NotFound(_))));
        assert!(matches!(get("keychain:other"), Err(Error::NotFound(_))));
        let broken = StoreResolver {
            secrets: Arc::new(NoKeyring),
            env: process_env(),
        };
        assert!(broken.resolve(&"keychain:x".parse().unwrap()).is_err());
    }
}
