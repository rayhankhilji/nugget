//! "Bring your own key": check a cloud (or self-hosted) provider key and save it.
//!
//! - `ai.key.test` asks the provider for its model list (`GET {base}/models` with a bearer token
//!   for the OpenAI kinds, `GET {base}/v1/models` with `x-api-key` for Anthropic) and classifies
//!   the answer: `ok`, `bad_key`, `wrong_url`, `no_network` or `server_error`. The outcome is a
//!   result, never a JSON-RPC error (only malformed params are).
//! - `ai.key.save` runs the same check, then stores the key with the daemon's [`SecretStore`]
//!   under [`provider_account`] and appends a `[[providers]]` table to `config.toml` that refers
//!   to it as `keychain:NAME` (the key itself never reaches the file). A provider with the same
//!   id is refused (`exists`). The router reads providers at start, so a restart is required.
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
use nugget_core::secrets::provider_account;
use nugget_core::{Error, Result};
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
const METHODS: &[&str] = &[AI_KEY_TEST, AI_KEY_SAVE];

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

/// `ai.key.test` params.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct KeyTestRequest {
    pub kind: Kind,
    #[serde(default)]
    pub base_url: Option<String>,
    pub api_key: ApiKey,
    #[serde(default)]
    pub model: Option<String>,
}

/// `ai.key.save` params.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct KeySaveRequest {
    pub name: String,
    pub kind: Kind,
    #[serde(default)]
    pub base_url: Option<String>,
    pub api_key: ApiKey,
    pub model: String,
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

/// `ai.key.save` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct KeySaveResult {
    pub ok: bool,
    pub reason: Reason,
    pub message: String,
    pub models: Vec<String>,
    pub saved: bool,
    pub provider_id: Option<String>,
    pub restart_required: bool,
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
            restart_required: true,
        }
    }
}

/// The `ai_keys` feature.
pub(crate) fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    let egress: Arc<dyn EgressSink> = Arc::new(cx.state.store.clone());
    Ok(Arc::new(AiKeys {
        state: cx.state.clone(),
        http: metered(egress)?,
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
                let result = check_key(&self.http, &req, CHECK_TIMEOUT).await;
                self.flush_egress().await;
                to_result(result)
            }
            AI_KEY_SAVE => {
                let req: KeySaveRequest = parse_params(params)?;
                validate_save(&req).map_err(RpcError::invalid_params)?;
                let result = self.save(req).await?;
                to_result(result)
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

    async fn save(&self, req: KeySaveRequest) -> Result<KeySaveResult> {
        let config_path = self.state.paths.config_file();
        // Refuse a taken id before going to the network (checked again when writing).
        if provider_exists(&config_path, &req.name)? {
            return Ok(exists_result(&req.name));
        }
        let test = KeyTestRequest {
            kind: req.kind,
            base_url: req.base_url.clone(),
            api_key: req.api_key.clone(),
            model: Some(req.model.clone()),
        };
        let checked = check_key(&self.http, &test, CHECK_TIMEOUT).await;
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
        };
        let key = req.api_key.0.trim().to_owned();
        let written = {
            let entry = entry.clone();
            blocking(move || write_provider(&config_path, &*secrets, &entry, &key)).await?
        };
        if written == Written::Exists {
            return Ok(exists_result(&req.name));
        }
        tracing::info!(provider = %entry.name, kind = entry.kind.as_str(), "provider key saved");
        let store = self.state.store.clone();
        let audit = AuditEntry {
            actor: "user".into(),
            action: "ai.key.saved".into(),
            target: entry.name.clone(),
            detail: json!({
                "kind": entry.kind.as_str(),
                "base_url": entry.base_url,
                "model": entry.model,
            }),
        };
        if let Err(e) = blocking(move || store.append_audit(&audit)).await {
            tracing::warn!(error = %e, "cannot append audit entry");
        }
        Ok(KeySaveResult {
            ok: true,
            reason: Reason::Ok,
            message: "Key checked and saved. Restart Nugget to start using it.".into(),
            models: checked.models,
            saved: true,
            provider_id: Some(entry.name),
            restart_required: true,
        })
    }
}

fn exists_result(name: &str) -> KeySaveResult {
    KeySaveResult::from_test(KeyTestResult::fail(
        Reason::Exists,
        format!("A provider named \"{name}\" is already set up. Pick another name."),
    ))
}

/// Name and model checks for `ai.key.save` (malformed params, not a validation outcome).
fn validate_save(req: &KeySaveRequest) -> std::result::Result<(), String> {
    let n = &req.name;
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
    let m = req.model.trim();
    if m.is_empty() || m.len() > 200 || m.chars().any(char::is_control) {
        return Err("model must be a model name of at most 200 characters".into());
    }
    Ok(())
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

/// Ask the provider for its models with `key` and classify the answer. Never fails: every
/// outcome is a [`KeyTestResult`].
pub(crate) async fn check_key(
    http: &MeteredHttp,
    req: &KeyTestRequest,
    timeout: Duration,
) -> KeyTestResult {
    let key = req.api_key.0.trim();
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
        t.insert("api_key".into(), format!("keychain:{}", self.name).into());
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
}

/// Whether `config.toml` at `path` already has a provider `name` (a missing file has none).
fn provider_exists(path: &Path, name: &str) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
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
}

/// Store `key` under [`provider_account`]`(name)` and append the provider to `config.toml`
/// (written whole to a temporary file, then renamed). The new file is validated as a [`Config`]
/// before anything is stored. A missing config starts from [`DEFAULT_CONFIG`].
pub(crate) fn write_provider(
    path: &Path,
    secrets: &dyn SecretStore,
    entry: &NewProvider,
    key: &str,
) -> Result<Written> {
    if provider_exists(path, &entry.name)? {
        return Ok(Written::Exists);
    }
    let mut text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_CONFIG.to_owned(),
        Err(e) => return Err(e.into()),
    };
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(
        "\n# Added by Nugget's AI setup. The key is in the OS keychain, not in this file.\n",
    );
    text.push_str(&entry.toml()?);
    Config::parse(&text)
        .map_err(|e| Error::Invalid(format!("the new provider is invalid: {e}")))?;

    let account = provider_account(&entry.name);
    secrets.set(&account, key)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    let written = std::fs::write(&tmp, &text).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        let _ = secrets.delete(&account);
        return Err(e.into());
    }
    Ok(Written::Saved)
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

    fn req(kind: Kind, base: Option<&str>, key: &str) -> KeyTestRequest {
        KeyTestRequest {
            kind,
            base_url: base.map(str::to_owned),
            api_key: ApiKey(key.to_owned()),
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
            api_key: ApiKey(GOOD.into()),
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
    fn write_provider_refers_to_the_keychain_and_refuses_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, DEFAULT_CONFIG).unwrap();
        let secrets = MemorySecrets::new();
        let entry = NewProvider {
            name: "custom".into(),
            kind: Kind::OpenaiCompatible,
            base_url: Some("http://127.0.0.1:9/v1".into()),
            model: "model \"a\"".into(),
        };
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
        assert!(cfg.providers.iter().any(|p| p.id.as_str() == "local"));
        assert_eq!(
            secrets.get(&provider_account("custom")).unwrap().as_deref(),
            Some(GOOD)
        );

        // The same id again (and the default `local`) is refused, and nothing changes.
        let other = MemorySecrets::new();
        assert_eq!(
            write_provider(&path, &other, &entry, "sk-other").unwrap(),
            Written::Exists
        );
        let local = NewProvider {
            name: "local".into(),
            ..entry.clone()
        };
        assert_eq!(
            write_provider(&path, &other, &local, "sk-other").unwrap(),
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
        };
        write_provider(&path, &secrets, &entry, GOOD).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("base_url = \"https://api.openai"));
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.providers.len(), 2);
    }
}
