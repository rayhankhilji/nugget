use std::sync::Arc;
use std::time::Duration;

use nugget_connector::http::{MeteredHttp, Response};
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
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
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
        for (what, u) in [("auth", &provider.auth_url), ("token", &provider.token_url)] {
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
        let redirect_uri = loopback.redirect_uri();
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
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("scope", &self.provider.scopes.join(" "))
                .append_pair("state", state)
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

fn parse_token_response(resp: Response) -> Result<TokenSet> {
    let status = resp.status.as_u16();
    if resp.status.is_success() {
        let tr: TokenResponse = serde_json::from_slice(&resp.body)
            .map_err(|_| Error::Invalid("token endpoint returned malformed JSON".into()))?;
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
                .map(|s| now.saturating_add(s.clamp(0, 10 * 365 * 86_400))),
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
        (429, _) => Error::RateLimited {
            retry_after_ms: resp
                .headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(|s| s.saturating_mul(1000)),
        },
        (500..=599, _) => Error::Network(format!("token endpoint: HTTP {status}")),
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
