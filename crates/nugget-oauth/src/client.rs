use std::sync::Arc;
use std::time::Duration;

use nugget_connector::http::{MeteredHttp, Response, retry_after_ms};
use nugget_connector::{AuthFlow, SecretStore};
use nugget_core::{Error, Result};
use reqwest::Method;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::Deserialize;
use url::Url;

use crate::loopback::Loopback;
use crate::pkce::{Pkce, random_token};
use crate::token::{TokenSet, store_tokens};
use crate::{ClientCredentials, DEFAULT_AUTH_TIMEOUT, Provider};

/// Options for one interactive authorization.
#[derive(Debug, Clone)]
pub struct AuthorizeOptions {
    /// How long to wait for the browser redirect (default 5 minutes).
    pub timeout: Duration,
    /// Pre-selects the account on the consent screen (`login_hint`).
    pub login_hint: Option<String>,
}

impl Default for AuthorizeOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_AUTH_TIMEOUT,
            login_hint: None,
        }
    }
}

/// An OAuth client for one provider and one user-supplied client id.
#[derive(Clone)]
pub struct OAuthClient {
    provider: Arc<Provider>,
    creds: Arc<ClientCredentials>,
    http: MeteredHttp,
}

impl std::fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClient")
            .field("provider", &self.provider.name)
            .field("client", &self.creds)
            .finish_non_exhaustive()
    }
}

/// An authorization waiting for the browser: open [`url`](Self::url), then
/// [`finish`](Self::finish).
pub struct Authorization {
    pub url: String,
    pub redirect_uri: String,
    client: OAuthClient,
    loopback: Loopback,
    state: String,
    pkce: Pkce,
    timeout: Duration,
}

impl std::fmt::Debug for Authorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorization")
            .field("url", &self.url)
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

impl Authorization {
    /// Wait for the redirect (one callback; `state` validated), then exchange the code.
    pub async fn finish(self) -> Result<TokenSet> {
        let code = self.loopback.wait(&self.state, self.timeout).await?;
        self.client
            .exchange_code(&code, &self.pkce.verifier, &self.redirect_uri)
            .await
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    /// Slack: `false` on errors (sent with HTTP 200).
    ok: Option<bool>,
    error: Option<String>,
    /// Slack: user tokens come here instead of at the top level.
    authed_user: Option<Box<TokenResponse>>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    /// Seconds. Some providers (Azure AD v1, ADFS) send it as a string, some as a float.
    #[serde(default)]
    expires_in: Option<serde_json::Value>,
    token_type: Option<String>,
    scope: Option<String>,
}

#[derive(Deserialize, Default)]
struct TokenError {
    error: Option<String>,
    error_description: Option<String>,
}

impl OAuthClient {
    /// `http` should be the connector's metered client (e.g. feature `sync:gmail`).
    pub fn new(provider: Provider, creds: ClientCredentials, http: MeteredHttp) -> Result<Self> {
        if creds.client_id.trim().is_empty() {
            return Err(Error::Invalid("OAuth client_id is empty".into()));
        }
        if !matches!(
            provider.redirect_host.as_deref(),
            None | Some("127.0.0.1" | "localhost")
        ) {
            return Err(Error::Invalid(
                "the redirect host must be 127.0.0.1 or localhost".into(),
            ));
        }
        let revoke = provider.revoke_url.iter().map(|u| ("revoke", u));
        for (what, u) in [("auth", &provider.auth_url), ("token", &provider.token_url)]
            .into_iter()
            .chain(revoke)
        {
            let parsed = Url::parse(u)
                .map_err(|e| Error::Invalid(format!("bad {what} endpoint `{u}`: {e}")))?;
            if !matches!(parsed.scheme(), "https" | "http") {
                return Err(Error::Invalid(format!("{what} endpoint must be http(s)")));
            }
        }
        Ok(Self {
            provider: Arc::new(provider),
            creds: Arc::new(creds),
            http,
        })
    }

    pub fn provider(&self) -> &Provider {
        &self.provider
    }

    pub fn client_id(&self) -> &str {
        &self.creds.client_id
    }

    /// Bind the loopback listener and build the authorization URL.
    pub async fn authorize(&self, opts: &AuthorizeOptions) -> Result<Authorization> {
        let loopback = Loopback::bind().await?;
        let host = self
            .provider
            .redirect_host
            .as_deref()
            .unwrap_or("127.0.0.1");
        let redirect_uri = loopback.redirect_uri(host);
        let state = random_token(32)?;
        let pkce = Pkce::generate()?;
        let url =
            self.authorization_url(&redirect_uri, &state, &pkce, opts.login_hint.as_deref())?;
        Ok(Authorization {
            url,
            redirect_uri,
            client: self.clone(),
            loopback,
            state,
            pkce,
            timeout: opts.timeout,
        })
    }

    /// The authorization URL for the given redirect, `state` and PKCE pair.
    pub fn authorization_url(
        &self,
        redirect_uri: &str,
        state: &str,
        pkce: &Pkce,
        login_hint: Option<&str>,
    ) -> Result<String> {
        let mut url = Url::parse(&self.provider.auth_url)
            .map_err(|e| Error::Invalid(format!("bad auth endpoint: {e}")))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("response_type", "code")
                .append_pair("client_id", &self.creds.client_id)
                .append_pair("redirect_uri", redirect_uri);
            // Providers with their own scope parameter (Slack's `user_scope`) set it in
            // `extra_auth_params` and leave `scopes` empty.
            if !self.provider.scopes.is_empty() {
                q.append_pair("scope", &self.provider.scopes.join(" "));
            }
            q.append_pair("state", state)
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", pkce.method());
            for (k, v) in &self.provider.extra_auth_params {
                q.append_pair(k, v);
            }
            if let Some(hint) = login_hint.filter(|h| !h.is_empty()) {
                q.append_pair("login_hint", hint);
            }
        }
        Ok(url.into())
    }

    /// Interactive authorization as a connector [`AuthFlow`]: when the user approves, the tokens
    /// are stored as JSON under `account` (use `nugget_core::secrets::oauth_account`).
    pub async fn begin(
        &self,
        secrets: Arc<dyn SecretStore>,
        account: impl Into<String>,
        opts: AuthorizeOptions,
    ) -> Result<AuthFlow> {
        let auth = self.authorize(&opts).await?;
        let url = auth.url.clone();
        let account = account.into();
        let done = async move {
            let tokens = auth.finish().await?;
            store_tokens(secrets, account, &tokens).await
        };
        Ok(AuthFlow {
            url,
            user_code: None,
            done: Box::pin(done),
        })
    }

    /// Exchange an authorization code (with its PKCE verifier) for tokens.
    pub async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenSet> {
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ];
        self.token_request(&mut form).await
    }

    /// Revoke the grant behind `token` (a refresh token where there is one) at the provider's
    /// revocation endpoint. `Ok(false)` when the provider has none for public clients. An
    /// already invalid token counts as revoked.
    pub async fn revoke(&self, token: &str) -> Result<bool> {
        let Some(url) = &self.provider.revoke_url else {
            return Ok(false);
        };
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("token", token)
            .append_pair("client_id", &self.creds.client_id)
            .finish();
        let req = self
            .http
            .request(Method::POST, url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json")
            .body(body);
        let resp = self.http.send(req, &[]).await?;
        let status = resp.status.as_u16();
        let err: TokenError = serde_json::from_slice(&resp.body).unwrap_or_default();
        let code = err.error.unwrap_or_default();
        // Slack answers HTTP 200 with `{"ok": false, "error": ...}`.
        let refused = serde_json::from_slice::<serde_json::Value>(&resp.body)
            .ok()
            .and_then(|v| v.get("ok").and_then(serde_json::Value::as_bool))
            == Some(false);
        // A token that is already dead needs no revoking.
        let dead = matches!(
            code.as_str(),
            "invalid_token" | "invalid_grant" | "invalid_auth" | "token_revoked" | "token_expired"
        );
        match status {
            200..=299 if !refused || dead => Ok(true),
            400 if dead => Ok(true),
            429 => Err(Error::RateLimited {
                retry_after_ms: retry_after_ms(&resp.headers),
            }),
            _ => Err(Error::Network(format!(
                "{} did not revoke the token (HTTP {status} {})",
                self.provider.name,
                code.chars().take(64).collect::<String>()
            ))),
        }
    }

    /// Use a refresh token. The returned set keeps `refresh_token` only if the provider sent a
    /// new one; [`TokenSource`](crate::TokenSource) carries the old one over.
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenSet> {
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ];
        self.token_request(&mut form).await
    }

    async fn token_request<'a>(&'a self, form: &mut Vec<(&'a str, &'a str)>) -> Result<TokenSet> {
        form.push(("client_id", &self.creds.client_id));
        if let Some(secret) = &self.creds.client_secret {
            form.push(("client_secret", secret));
        }
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form.iter())
            .finish();
        let req = self
            .http
            .request(Method::POST, &self.provider.token_url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json")
            .body(body);
        let resp = self.http.send(req, &[]).await?;
        parse_token_response(resp)
    }
}

/// Slack reports token errors as HTTP 200 with `{"ok": false, "error": "..."}`.
fn slack_token_error(code: &str) -> Error {
    let code: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect();
    match code.as_str() {
        "invalid_refresh_token"
        | "invalid_grant_type"
        | "invalid_code"
        | "code_expired"
        | "code_already_used"
        | "token_revoked"
        | "token_expired"
        | "invalid_auth" => Error::Unauthorized(format!(
            "the authorization was revoked or expired ({code}); reconnect the account"
        )),
        "invalid_client_id" | "bad_client_secret" | "bad_redirect_uri" => Error::Unauthorized(
            format!("the OAuth client was rejected ({code}); check the client ID"),
        ),
        "ratelimited" => Error::RateLimited {
            retry_after_ms: None,
        },
        _ => Error::Other(format!("token endpoint: {code}")),
    }
}

/// Longest token lifetime believed from `expires_in` (servers occasionally send absurd values).
const MAX_EXPIRES_IN: i64 = 10 * 365 * 86_400;

/// `expires_in` as whole seconds, clamped to `0..=MAX_EXPIRES_IN`. A number, a numeric string
/// or a float are accepted; anything else (or a non-positive value, which would make every call
/// refresh) means "the provider did not say", so the token is used until the API rejects it.
fn expires_in_secs(v: &serde_json::Value) -> Option<i64> {
    let secs = match v {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))?,
        serde_json::Value::String(s) => {
            s.trim().parse::<f64>().ok().filter(|f| f.is_finite())? as i64
        }
        _ => return None,
    };
    (secs > 0).then(|| secs.min(MAX_EXPIRES_IN))
}

fn parse_token_response(resp: Response) -> Result<TokenSet> {
    let status = resp.status.as_u16();
    let slack_error = resp
        .status
        .is_success()
        .then(|| serde_json::from_slice::<TokenResponse>(&resp.body).ok())
        .flatten()
        .filter(|tr| tr.ok == Some(false))
        .map(|tr| tr.error.unwrap_or_default());
    if let Some(code) = slack_error {
        return Err(slack_token_error(&code));
    }
    if resp.status.is_success() {
        let mut tr: TokenResponse = serde_json::from_slice(&resp.body)
            .map_err(|_| Error::Invalid("token endpoint returned malformed JSON".into()))?;
        // Slack user-token flows: the tokens are under `authed_user`.
        if tr.access_token.is_none()
            && let Some(user) = tr.authed_user.take()
        {
            tr = *user;
        }
        let access_token = tr
            .access_token
            .filter(|t| !t.is_empty())
            .ok_or_else(|| Error::Invalid("token endpoint returned no access_token".into()))?;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        return Ok(TokenSet {
            access_token,
            refresh_token: tr.refresh_token.filter(|t| !t.is_empty()),
            expires_at: tr
                .expires_in
                .as_ref()
                .and_then(expires_in_secs)
                .map(|s| now.saturating_add(s)),
            token_type: tr.token_type,
            scope: tr.scope,
        });
    }
    // Never echo the body: report only the OAuth error code and description.
    let err: TokenError = serde_json::from_slice(&resp.body).unwrap_or_default();
    let code = err.error.unwrap_or_default();
    let desc: String = err
        .error_description
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    Err(match (status, code.as_str()) {
        (_, "invalid_grant") => Error::Unauthorized(format!(
            "the authorization was revoked or expired (invalid_grant); reconnect the account{}",
            if desc.is_empty() {
                String::new()
            } else {
                format!(": {desc}")
            }
        )),
        (_, "invalid_client" | "unauthorized_client") => Error::Unauthorized(format!(
            "the OAuth client was rejected ({code}); check client_id and client_secret"
        )),
        (300..=399, _) => Error::Network(format!(
            "unexpected redirect from token endpoint (HTTP {status})"
        )),
        // Transient: the provider is overloaded or down. Never `Unauthorized` (that would make
        // the user reconnect over an outage). `Retry-After` may be seconds or an HTTP-date.
        (429, _) | (_, "slow_down") => Error::RateLimited {
            retry_after_ms: retry_after_ms(&resp.headers),
        },
        (500..=599, _) if retry_after_ms(&resp.headers).is_some() => Error::RateLimited {
            retry_after_ms: retry_after_ms(&resp.headers),
        },
        (500..=599, _) | (_, "temporarily_unavailable" | "server_error") => {
            Error::Network(format!("token endpoint: HTTP {status} {code}"))
        }
        (401 | 403, _) => Error::Unauthorized(format!("token endpoint: HTTP {status} {code}")),
        _ => Error::Other(format!(
            "token endpoint: HTTP {status} {code}{}",
            if desc.is_empty() {
                String::new()
            } else {
                format!(": {desc}")
            }
        )),
    })
}
