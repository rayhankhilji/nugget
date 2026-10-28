//! Authenticated, metered, rate-limit-aware GET requests against Google APIs.

use std::sync::Arc;
use std::time::Duration;

use nugget_connector::http::{MeteredHttp, Response};
use nugget_connector::{AuthFlow, CancellationToken, ConnectorContext};
use nugget_core::secrets::oauth_account;
use nugget_core::{Error, Result};
use nugget_oauth::{AuthorizeOptions, ClientCredentials, OAuthClient, TokenSource};
use reqwest::Method;
use reqwest::header::ACCEPT;
use serde::Deserialize;
use url::Url;

use crate::{GoogleEndpoints, RetryPolicy};

/// Google 403 reasons that mean "slow down" rather than "not allowed".
const RATE_REASONS: &[&str] = &["rateLimitExceeded", "userRateLimitExceeded"];
/// Google 403 reasons for an exhausted daily quota: back off for a long time, no inline retry.
const QUOTA_REASONS: &[&str] = &["dailyLimitExceeded", "quotaExceeded"];

/// The user's OAuth client, from source settings.
pub(crate) struct OAuthSettings {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub login_hint: Option<String>,
}

pub(crate) struct GoogleApi {
    http: MeteredHttp,
    tokens: Arc<TokenSource>,
    retry: RetryPolicy,
    secrets_ctx: ConnectorContext,
    login_hint: Option<String>,
}

enum Outcome {
    /// 2xx, 404 and 410: the caller decides.
    Done(Response),
    /// 401: refresh the token once and retry.
    Refresh,
    /// Worth retrying after a backoff.
    Retry(Error),
    Fatal(Error),
}

#[derive(Deserialize, Default)]
struct GoogleError {
    #[serde(default)]
    error: GoogleErrorBody,
}

#[derive(Deserialize, Default)]
struct GoogleErrorBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    errors: Vec<GoogleErrorItem>,
    #[serde(default)]
    details: Vec<GoogleErrorDetail>,
}

#[derive(Deserialize, Default)]
struct GoogleErrorItem {
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize, Default)]
struct GoogleErrorDetail {
    #[serde(default)]
    reason: String,
}

impl GoogleApi {
    pub(crate) fn new(
        feature: &str,
        scope: &str,
        client: OAuthSettings,
        endpoints: &GoogleEndpoints,
        retry: RetryPolicy,
        ctx: ConnectorContext,
    ) -> Result<Self> {
        let OAuthSettings {
            client_id,
            client_secret,
            login_hint,
        } = client;
        let http = MeteredHttp::new(ctx.egress.clone(), feature)?;
        let provider = nugget_oauth::google::provider_with_endpoints(
            &endpoints.auth_url,
            &endpoints.token_url,
            &[scope],
        );
        let creds = ClientCredentials::new(client_id.trim(), client_secret);
        let client = OAuthClient::new(provider, creds, http.clone())?;
        let tokens = TokenSource::new(
            client,
            ctx.secrets.clone(),
            oauth_account(ctx.source.as_str()),
        );
        Ok(Self {
            http,
            tokens: Arc::new(tokens),
            retry,
            secrets_ctx: ctx,
            login_hint: login_hint.filter(|h| !h.trim().is_empty()),
        })
    }

    /// The interactive OAuth flow; on success the token cache is reset.
    pub(crate) async fn begin_auth(&self) -> Result<AuthFlow> {
        let opts = AuthorizeOptions {
            login_hint: self.login_hint.clone(),
            ..Default::default()
        };
        let flow = self
            .tokens
            .client()
            .begin(
                self.secrets_ctx.secrets.clone(),
                oauth_account(self.secrets_ctx.source.as_str()),
                opts,
            )
            .await?;
        let tokens = self.tokens.clone();
        let done = flow.done;
        Ok(AuthFlow {
            url: flow.url,
            done: Box::pin(async move {
                done.await?;
                tokens.clear_cache().await;
                Ok(())
            }),
        })
    }

    /// Fail fast with `Unauthorized` when no tokens are stored.
    pub(crate) async fn ensure_token(&self) -> Result<()> {
        self.tokens.access_token().await.map(|_| ())
    }

    /// GET `url` as JSON. Returns 2xx, 404 and 410 responses; maps the rest to typed errors.
    /// 401 refreshes the token once; 429, rate-limit 403s, 408 and 5xx are retried with
    /// exponential backoff (honouring `Retry-After`) up to the policy's limit.
    pub(crate) async fn get(&self, url: &Url, cancel: &CancellationToken) -> Result<Response> {
        let mut attempt = 0u32;
        let mut refreshed = false;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let token = self.tokens.access_token().await?;
            let req = self
                .http
                .request(Method::GET, url.as_str())
                .bearer_auth(&token)
                .header(ACCEPT, "application/json");
            let outcome = match self.http.send(req, &[]).await {
                Ok(resp) => classify(resp),
                Err(e @ Error::Network(_)) => Outcome::Retry(e),
                Err(e) => Outcome::Fatal(e),
            };
            match outcome {
                Outcome::Done(resp) => return Ok(resp),
                Outcome::Refresh if !refreshed => {
                    refreshed = true;
                    self.tokens.force_refresh(&token).await?;
                }
                Outcome::Refresh => {
                    return Err(Error::Unauthorized(
                        "Google rejected the access token; reconnect the account".into(),
                    ));
                }
                Outcome::Retry(e) if attempt < self.retry.max_retries => {
                    let delay = self.backoff(attempt, &e);
                    tracing::debug!(%url, attempt, ?delay, "google api: retrying after {e}");
                    attempt += 1;
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = cancel.cancelled() => return Err(Error::Cancelled),
                    }
                }
                Outcome::Retry(e) | Outcome::Fatal(e) => return Err(e),
            }
        }
    }

    fn backoff(&self, attempt: u32, err: &Error) -> Duration {
        let exp = self
            .retry
            .base_delay
            .saturating_mul(1u32 << attempt.min(16));
        let hinted = match err {
            Error::RateLimited {
                retry_after_ms: Some(ms),
            } => Duration::from_millis(*ms),
            _ => Duration::ZERO,
        };
        let base = exp.max(hinted).min(self.retry.max_delay);
        // Up to 25% jitter so parallel sources do not retry in lockstep.
        let nanos = time::OffsetDateTime::now_utc().nanosecond() as u64;
        let jitter = base.as_millis() as u64 / 4;
        base + Duration::from_millis(if jitter == 0 { 0 } else { nanos % jitter })
    }

    /// Parse a JSON body, reporting malformed input as `Invalid` (never panics).
    pub(crate) fn json<T: for<'de> Deserialize<'de>>(resp: &Response, what: &str) -> Result<T> {
        serde_json::from_slice(&resp.body)
            .map_err(|e| Error::Invalid(format!("malformed {what} response: {e}")))
    }
}

fn classify(resp: Response) -> Outcome {
    let status = resp.status.as_u16();
    if resp.status.is_success() || status == 404 || status == 410 {
        return Outcome::Done(resp);
    }
    let retry_after_ms = resp
        .headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|s| s.saturating_mul(1000));
    let body: GoogleError = serde_json::from_slice(&resp.body).unwrap_or_default();
    let reasons: Vec<&str> = body
        .error
        .errors
        .iter()
        .map(|e| e.reason.as_str())
        .chain(body.error.details.iter().map(|d| d.reason.as_str()))
        .collect();
    let has = |set: &[&str]| reasons.iter().any(|r| set.contains(r));
    let message: String = body.error.message.chars().take(300).collect();
    match status {
        401 => Outcome::Refresh,
        429 => Outcome::Retry(Error::RateLimited { retry_after_ms }),
        403 if has(RATE_REASONS) => Outcome::Retry(Error::RateLimited { retry_after_ms }),
        403 if has(QUOTA_REASONS) => Outcome::Fatal(Error::RateLimited {
            retry_after_ms: Some(retry_after_ms.unwrap_or(3_600_000)),
        }),
        403 => Outcome::Fatal(Error::Unauthorized(format!(
            "Google denied access (HTTP 403{}): {message}",
            reasons
                .first()
                .map(|r| format!(", {r}"))
                .unwrap_or_default()
        ))),
        300..=399 => Outcome::Fatal(Error::Network(format!(
            "unexpected redirect from Google (HTTP {status})"
        ))),
        408 | 500..=599 => {
            Outcome::Retry(Error::Network(format!("Google HTTP {status}: {message}")))
        }
        400 => Outcome::Fatal(Error::Invalid(format!(
            "Google rejected the request: {message}"
        ))),
        _ => Outcome::Fatal(Error::Other(format!("Google HTTP {status}: {message}"))),
    }
}

/// Stops a listing that never ends: a page token the server already handed out in this loop
/// means it is going in circles (a broken or hostile server), and the pass fails instead of
/// spinning, re-fetching and re-checkpointing the same pages forever.
#[derive(Default)]
pub(crate) struct PageGuard(std::collections::HashSet<String>);

impl PageGuard {
    /// Record `token` (the next page to request); an error if it was seen before.
    pub(crate) fn next(&mut self, token: &str, what: &str) -> Result<()> {
        if self.0.insert(token.to_string()) {
            Ok(())
        } else {
            Err(Error::Network(format!(
                "{what}: the server repeated a page token; stopping the listing"
            )))
        }
    }
}

/// `<base>/<segments...>?<query>`, with each segment percent-encoded.
pub(crate) fn api_url(base: &str, segments: &[&str], query: &[(&str, &str)]) -> Result<Url> {
    let mut url =
        Url::parse(base).map_err(|e| Error::Invalid(format!("bad API base `{base}`: {e}")))?;
    url.path_segments_mut()
        .map_err(|_| Error::Invalid(format!("bad API base `{base}`")))?
        .pop_if_empty()
        .extend(segments);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Ok(url)
}
