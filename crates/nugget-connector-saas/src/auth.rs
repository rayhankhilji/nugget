//! Credentials for the SaaS connectors, and the OAuth presets that live in this crate.
//!
//! A connector looks for its token, in order:
//! 1. `token_env` from settings: the named environment variable (headless setups). If the
//!    setting is present but the variable is unset, the pass is `Unauthorized`.
//! 2. `oauth:<source>`: a token set stored by `begin_auth` (GitHub device flow, Linear PKCE).
//!    It is refreshed through [`TokenSource`] when it expires and an OAuth client is configured.
//! 3. `password:<source>`: a token pasted through `sources.secret.set` (Slack user token, Notion
//!    integration token, GitHub fine-grained PAT, Linear personal API key).
//!
//! Nothing found is `Error::Unauthorized`. Tokens are never logged or put in `Debug` output.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use nugget_connector::http::MeteredHttp;
use nugget_connector::{AuthFlow, SecretStore};
use nugget_core::secrets::{oauth_account, password_account};
use nugget_core::{Error, Result};
use nugget_oauth::{ClientCredentials, OAuthClient, Provider, TokenSet, TokenSource};
use reqwest::Method;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::Deserialize;

/// Keychain account of a source's OAuth client secret (same name the daemon uses for OAuth
/// kinds). Read when settings carry no `client_secret`.
pub fn client_secret_account(source: &str) -> String {
    format!("oauth_client_secret:{source}")
}

/// Linear OAuth 2.0 (authorization code with PKCE; the client secret is optional with PKCE).
pub mod linear {
    use nugget_oauth::Provider;

    pub const AUTH_URL: &str = "https://linear.app/oauth/authorize";
    pub const TOKEN_URL: &str = "https://api.linear.app/oauth/token";
    /// Read-only access to the workspace the user picks.
    pub const SCOPE_READ: &str = "read";

    pub fn provider_with_endpoints(auth_url: &str, token_url: &str) -> Provider {
        Provider {
            name: "linear".into(),
            auth_url: auth_url.into(),
            token_url: token_url.into(),
            scopes: vec![SCOPE_READ.into()],
            extra_auth_params: vec![("prompt".into(), "consent".into())],
        }
    }
}

/// GitHub OAuth: the device flow (no client secret, no redirect listener), and the token
/// endpoint for refreshing GitHub App user tokens.
pub mod github {
    use nugget_oauth::Provider;

    pub const LOGIN_ROOT: &str = "https://github.com/login";
    /// Read access to the user's repositories (OAuth Apps; GitHub Apps use their permissions).
    pub const SCOPES: &[&str] = &["repo", "read:discussion"];

    /// Used only for refreshing expiring GitHub App user tokens.
    pub fn provider(login_root: &str) -> Provider {
        let b = login_root.trim_end_matches('/');
        Provider {
            name: "github".into(),
            auth_url: format!("{b}/oauth/authorize"),
            token_url: format!("{b}/oauth/access_token"),
            scopes: SCOPES.iter().map(|s| (*s).to_string()).collect(),
            extra_auth_params: Vec::new(),
        }
    }
}

/// A resolved access token and where it came from.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Token {
    /// Pasted (personal) token or one from `token_env`.
    Pasted(String),
    /// An OAuth access token.
    OAuth(String),
}

impl Token {
    pub(crate) fn secret(&self) -> &str {
        match self {
            Token::Pasted(t) | Token::OAuth(t) => t,
        }
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Token::Pasted(_) => "Token::Pasted(<redacted>)",
            Token::OAuth(_) => "Token::OAuth(<redacted>)",
        })
    }
}

#[derive(Clone)]
enum Resolved {
    Pasted(String),
    /// OAuth tokens with a client that can refresh them.
    Refreshable,
    /// OAuth tokens and no client configured: used as stored until they expire.
    Fixed(String),
}

/// Finds and caches one source's token (see the module docs for the lookup order).
pub(crate) struct Credential {
    service: &'static str,
    secrets: Arc<dyn SecretStore>,
    source: String,
    token_env: Option<String>,
    oauth: Option<Arc<TokenSource>>,
    cache: Mutex<Option<Resolved>>,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("service", &self.service)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

async fn keychain_get(secrets: &Arc<dyn SecretStore>, account: String) -> Result<Option<String>> {
    let secrets = secrets.clone();
    tokio::task::spawn_blocking(move || secrets.get(&account))
        .await
        .map_err(|e| Error::Other(format!("keychain task failed: {e}")))?
}

impl Credential {
    pub(crate) fn new(
        service: &'static str,
        secrets: Arc<dyn SecretStore>,
        source: &str,
        token_env: Option<String>,
        oauth: Option<Arc<TokenSource>>,
    ) -> Self {
        Self {
            service,
            secrets,
            source: source.to_string(),
            token_env,
            oauth,
            cache: Mutex::new(None),
        }
    }

    /// Forget the resolved credential (start of a pass, or after a new authorization).
    pub(crate) fn reset(&self) {
        if let Ok(mut c) = self.cache.lock() {
            *c = None;
        }
    }

    fn cached(&self) -> Option<Resolved> {
        self.cache.lock().ok().and_then(|c| c.clone())
    }

    fn remember(&self, r: Resolved) {
        if let Ok(mut c) = self.cache.lock() {
            *c = Some(r);
        }
    }

    /// The token to send. `Unauthorized` when none is configured.
    pub(crate) async fn token(&self) -> Result<Token> {
        let resolved = match self.cached() {
            Some(r) => r,
            None => {
                let r = self.resolve().await?;
                self.remember(r.clone());
                r
            }
        };
        match resolved {
            Resolved::Pasted(t) => Ok(Token::Pasted(t)),
            Resolved::Fixed(t) => Ok(Token::OAuth(t)),
            Resolved::Refreshable => match &self.oauth {
                Some(ts) => ts.access_token().await.map(Token::OAuth),
                None => Err(Error::Unauthorized(format!(
                    "{} is not connected",
                    self.service
                ))),
            },
        }
    }

    /// After the API rejected `rejected` (HTTP 401): a refreshed OAuth token, or
    /// `Unauthorized` when the token cannot be refreshed.
    pub(crate) async fn refresh(&self, rejected: &Token) -> Result<Token> {
        match (rejected, self.cached(), &self.oauth) {
            (Token::OAuth(t), Some(Resolved::Refreshable), Some(ts)) => {
                ts.force_refresh(t).await.map(Token::OAuth)
            }
            _ => {
                self.reset();
                Err(Error::Unauthorized(format!(
                    "{} rejected the token; paste a new one or reconnect the source",
                    self.service
                )))
            }
        }
    }

    async fn resolve(&self) -> Result<Resolved> {
        if let Some(var) = &self.token_env {
            return match std::env::var(var) {
                Ok(v) if !v.trim().is_empty() => Ok(Resolved::Pasted(v.trim().to_string())),
                _ => Err(Error::Unauthorized(format!(
                    "environment variable `{var}` (token_env) is not set"
                ))),
            };
        }
        if let Some(raw) = keychain_get(&self.secrets, oauth_account(&self.source)).await? {
            let tokens: TokenSet = serde_json::from_str(&raw).map_err(|_| {
                Error::Unauthorized(format!(
                    "stored {} OAuth tokens are unreadable; reconnect the source",
                    self.service
                ))
            })?;
            let refreshable = tokens.refresh_token.is_some() && self.oauth.is_some();
            if refreshable {
                return Ok(Resolved::Refreshable);
            }
            if tokens.expires_within(Duration::from_secs(30)) {
                return Err(Error::Unauthorized(format!(
                    "the {} OAuth token expired; reconnect the source",
                    self.service
                )));
            }
            return Ok(Resolved::Fixed(tokens.access_token));
        }
        match keychain_get(&self.secrets, password_account(&self.source)).await? {
            Some(t) if !t.trim().is_empty() => Ok(Resolved::Pasted(t.trim().to_string())),
            _ => Err(Error::Unauthorized(format!(
                "no {} token is set; paste one with sources.secret.set",
                self.service
            ))),
        }
    }
}

/// An OAuth client secret: from settings, else from `oauth_client_secret:<source>`.
pub(crate) fn client_secret(
    secrets: &Arc<dyn SecretStore>,
    source: &str,
    from_settings: Option<&str>,
) -> Option<String> {
    if let Some(s) = from_settings.filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    secrets
        .get(&client_secret_account(source))
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// A [`TokenSource`] for `provider` and the user's client id, stored under `oauth:<source>`.
pub(crate) fn token_source(
    provider: Provider,
    client_id: &str,
    secret: Option<String>,
    http: MeteredHttp,
    secrets: Arc<dyn SecretStore>,
    source: &str,
) -> Result<Arc<TokenSource>> {
    let client = OAuthClient::new(
        provider,
        ClientCredentials::new(client_id.trim(), secret),
        http,
    )?;
    Ok(Arc::new(TokenSource::new(
        client,
        secrets,
        oauth_account(source),
    )))
}

/// Wrap a flow so the credential cache is reset once it stored new tokens.
pub(crate) fn clearing(
    flow: AuthFlow,
    cred: Arc<Credential>,
    tokens: Option<Arc<TokenSource>>,
) -> AuthFlow {
    let done = flow.done;
    AuthFlow {
        url: flow.url,
        done: Box::pin(async move {
            done.await?;
            cred.reset();
            if let Some(t) = tokens {
                t.clear_cache().await;
            }
            Ok(())
        }),
    }
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Deserialize, Default)]
struct DeviceToken {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
}

/// Options for the GitHub device flow.
#[derive(Debug, Clone)]
pub(crate) struct DeviceFlow {
    pub login_root: String,
    pub client_id: String,
    /// Shortest wait between polls (GitHub's `interval` is honoured when longer).
    pub min_interval: Duration,
    /// Give up after this long even if GitHub's code is still valid.
    pub timeout: Duration,
}

/// Start GitHub's device flow (RFC 8628). The returned URL is GitHub's verification page with
/// the user code appended (`?user_code=...`), the only channel `AuthFlow` has to show it; `done`
/// polls the token endpoint and stores the token set under `oauth:<source>`.
pub(crate) async fn github_device_flow(
    http: &MeteredHttp,
    opts: DeviceFlow,
    secrets: Arc<dyn SecretStore>,
    source: &str,
) -> Result<AuthFlow> {
    let root = opts.login_root.trim_end_matches('/').to_string();
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &opts.client_id)
        .append_pair("scope", &github::SCOPES.join(" "))
        .finish();
    let req = http
        .request(Method::POST, &format!("{root}/device/code"))
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .body(body);
    let resp = http.send(req, &[]).await?.error_for_status()?;
    let code: DeviceCode = serde_json::from_slice(&resp.body)
        .map_err(|_| Error::Invalid("GitHub device/code returned malformed JSON".into()))?;
    let mut url = url::Url::parse(&code.verification_uri)
        .map_err(|_| Error::Invalid("GitHub returned a bad verification URL".into()))?;
    if url.scheme() != "https" && !root.starts_with("http://127.0.0.1") {
        return Err(Error::Invalid(
            "GitHub returned a non-https verification URL".into(),
        ));
    }
    if code.user_code.len() > 64 || code.user_code.chars().any(char::is_control) {
        return Err(Error::Invalid("GitHub returned a bad user code".into()));
    }
    url.query_pairs_mut()
        .append_pair("user_code", &code.user_code);
    let http = http.clone();
    let account = oauth_account(source);
    let expires =
        Duration::from_secs(code.expires_in.unwrap_or(900).clamp(60, 1800)).min(opts.timeout);
    let mut interval =
        Duration::from_secs(code.interval.unwrap_or(5).min(60)).max(opts.min_interval);
    let client_id = opts.client_id.clone();
    let device_code = code.device_code;
    let done = async move {
        let deadline = tokio::time::Instant::now() + expires;
        loop {
            // Dropping `done` (the daemon abandons the flow) stops the polling.
            tokio::time::sleep(interval).await;
            if tokio::time::Instant::now() > deadline {
                return Err(Error::Unauthorized(
                    "the GitHub device code expired before it was approved".into(),
                ));
            }
            let body = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("client_id", &client_id)
                .append_pair("device_code", &device_code)
                .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:device_code")
                .finish();
            let req = http
                .request(Method::POST, &format!("{root}/oauth/access_token"))
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(ACCEPT, "application/json")
                .body(body);
            let resp = http.send(req, &[]).await?;
            if resp.status.is_redirection() {
                return Err(Error::Network(
                    "unexpected redirect from the GitHub token endpoint".into(),
                ));
            }
            if resp.status.is_server_error() {
                continue;
            }
            let t: DeviceToken = serde_json::from_slice(&resp.body).unwrap_or_default();
            match t.error.as_deref() {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    let next = t
                        .interval
                        .map(Duration::from_secs)
                        .unwrap_or(interval + Duration::from_secs(5));
                    interval = next.min(Duration::from_secs(60)).max(opts.min_interval);
                    continue;
                }
                Some("expired_token") => {
                    return Err(Error::Unauthorized(
                        "the GitHub device code expired; start again".into(),
                    ));
                }
                Some("access_denied") => {
                    return Err(Error::Unauthorized(
                        "the GitHub authorization was denied".into(),
                    ));
                }
                Some(other) => {
                    let other: String = other.chars().take(64).collect();
                    return Err(Error::Unauthorized(format!(
                        "GitHub device flow failed: {other}"
                    )));
                }
                None => {}
            }
            let access_token = t
                .access_token
                .filter(|a| !a.is_empty())
                .ok_or_else(|| Error::Invalid("GitHub returned no access token".into()))?;
            let now = time::OffsetDateTime::now_utc().unix_timestamp();
            let tokens = TokenSet {
                access_token,
                refresh_token: t.refresh_token.filter(|r| !r.is_empty()),
                expires_at: t
                    .expires_in
                    .map(|s| now.saturating_add(s.clamp(0, 10 * 365 * 86_400))),
                token_type: t.token_type,
                scope: t.scope,
            };
            let json = serde_json::to_string(&tokens)?;
            let secrets = secrets.clone();
            let account = account.clone();
            return tokio::task::spawn_blocking(move || secrets.set(&account, &json))
                .await
                .map_err(|e| Error::Other(format!("keychain task failed: {e}")))?;
        }
    };
    Ok(AuthFlow {
        url: url.into(),
        done: Box::pin(done),
    })
}
