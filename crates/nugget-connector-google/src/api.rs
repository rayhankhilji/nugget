//! Authenticated, metered, rate-limit-aware GET requests against Google APIs.

use std::sync::Arc;

use nugget_connector::http::{MeteredHttp, Response};
use nugget_connector::retry;
use nugget_connector::{AuthFlow, CancellationToken, ConnectorContext};
use nugget_core::secrets::oauth_account;
use nugget_core::{Error, Result};
use nugget_oauth::{AuthorizeOptions, OAuthClient, TokenSource};
use reqwest::Method;
use reqwest::header::ACCEPT;
use serde::Deserialize;
use url::Url;

use crate::{GoogleEndpoints, RetryPolicy};

/// Google 403 reasons that mean "slow down" rather than "not allowed".
const RATE_REASONS: &[&str] = &["rateLimitExceeded", "userRateLimitExceeded"];
/// Google 403 reasons for an exhausted daily quota: back off for a long time, no inline retry.
const QUOTA_REASONS: &[&str] = &["dailyLimitExceeded", "quotaExceeded"];

/// The user's OAuth client, from source settings (absent: the built-in one).
pub(crate) struct OAuthSettings {
    pub client_id: Option<String>,
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

enum Outcome<T = Response> {
    /// 2xx, 404 and 410 (and, for [`GoogleApi::get_media`], per-file refusals): the caller
    /// decides.
    Done(T),
    /// 401: refresh the token once and retry.
    Refresh,
    /// Worth retrying after a backoff.
    Retry(Error),
    Fatal(Error),
}

/// The result of [`GoogleApi::get_media`].
#[derive(Debug)]
pub(crate) enum Media {
    /// A 2xx, 404 or 410 response: check `status`.
    Body(Response),
    /// The body was larger than the per-call cap.
    TooLarge,
    /// Google refused this one request for a reason that does not affect others: a 400, or a
    /// 403 that is neither a rate limit nor a quota (e.g. `cannotExportFile`,
    /// `exportSizeLimitExceeded`, `fileNotDownloadable`, `cannotDownloadAbusiveFile`, or
    /// `accessNotConfigured` / `SERVICE_DISABLED` when an API is not enabled for the client).
    Refused {
        status: u16,
        /// The first Google error reason (`errors[].reason`, then `details[].reason`), bounded
        /// to letters, digits and `_`; may be empty.
        reason: String,
        /// Google's error message, at most 300 characters. For logs, not for users.
        message: String,
    },
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
    errors: Vec<Reason>,
    #[serde(default)]
    details: Vec<Reason>,
}

#[derive(Deserialize, Default)]
struct Reason {
    #[serde(default)]
    reason: String,
}

/// A Google error body: its reasons (`errors[]` then `details[]`) and message (≤ 300 chars).
fn parse_error(body: &[u8]) -> (Vec<String>, String) {
    let e = serde_json::from_slice::<GoogleError>(body)
        .unwrap_or_default()
        .error;
    let reasons = e.errors.into_iter().chain(e.details).map(|r| r.reason);
    (reasons.collect(), e.message.chars().take(300).collect())
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
        let creds = nugget_oauth::bundled::resolve(
            client_id.as_deref(),
            client_secret,
            endpoints.builtin_client.clone(),
        )
        .ok_or_else(|| Error::Invalid(crate::settings::NO_CLIENT.into()))?;
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
            user_code: flow.user_code,
            done: Box::pin(async move {
                done.await?;
                tokens.clear_cache().await;
                Ok(())
            }),
        })
    }

    /// Revoke the grant at Google (best effort) and delete the stored tokens.
    pub(crate) async fn sign_out(&self) -> Result<()> {
        self.tokens.disconnect().await.map(|_| ())
    }

    /// Fail fast with `Unauthorized` when no tokens are stored.
    pub(crate) async fn ensure_token(&self) -> Result<()> {
        self.tokens.access_token().await.map(|_| ())
    }

    /// GET `url` as JSON. Returns 2xx, 404 and 410 responses; maps the rest to typed errors.
    /// 401 refreshes the token once; 429, rate-limit 403s, 408 and 5xx are retried with
    /// exponential backoff (honouring `Retry-After`) up to the policy's limit.
    pub(crate) async fn get(&self, url: &Url, cancel: &CancellationToken) -> Result<Response> {
        self.call(&self.http, Method::GET, url, None, cancel).await
    }

    /// [`get`](Self::get) with a per-call body cap: a larger body is `Error::Invalid("response
    /// body exceeds ...")`, so the caller can skip that one item.
    pub(crate) async fn get_capped(
        &self,
        url: &Url,
        max_body: usize,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        let http = self.http.clone().with_max_body(max_body);
        self.call(&http, Method::GET, url, None, cancel).await
    }

    /// POST a JSON body to `url`, with the same status handling and retries as [`get`](Self::get).
    pub(crate) async fn post_json(
        &self,
        url: &Url,
        body: &serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        self.call(&self.http, Method::POST, url, Some(body), cancel)
            .await
    }

    /// GET `url` for raw content (exports, downloads, or JSON whose errors must not fail a whole
    /// sync), with the same token refresh, retries and backoff as [`get`](Self::get), the given
    /// `Accept` header and a per-call body cap. 2xx, 404 and 410 are [`Media::Body`]; a body
    /// over `max_body` is [`Media::TooLarge`]; 400s and 403s other than rate limits and quota
    /// are [`Media::Refused`]. Rate limits, quota, 5xx, network failures and 401 after a
    /// refresh are errors exactly as in [`get`](Self::get).
    pub(crate) async fn get_media(
        &self,
        url: &Url,
        accept: &str,
        max_body: usize,
        cancel: &CancellationToken,
    ) -> Result<Media> {
        let http = self.http.clone().with_max_body(max_body);
        self.run(
            &http,
            Method::GET,
            url,
            None,
            accept,
            cancel,
            |sent| match sent {
                Ok(resp) => classify_media(resp),
                Err(Error::Invalid(m)) if m.starts_with("response body exceeds") => {
                    Outcome::Done(Media::TooLarge)
                }
                Err(e @ Error::Network(_)) => Outcome::Retry(e),
                Err(e) => Outcome::Fatal(e),
            },
        )
        .await
    }

    async fn call(
        &self,
        http: &MeteredHttp,
        method: Method,
        url: &Url,
        body: Option<&serde_json::Value>,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        self.run(
            http,
            method,
            url,
            body,
            "application/json",
            cancel,
            |sent| match sent {
                Ok(resp) => classify(resp),
                Err(e @ Error::Network(_)) => Outcome::Retry(e),
                Err(e) => Outcome::Fatal(e),
            },
        )
        .await
    }

    /// The request loop shared by every call: token, send, classify, refresh once on 401,
    /// retry with backoff.
    #[allow(clippy::too_many_arguments)]
    async fn run<T>(
        &self,
        http: &MeteredHttp,
        method: Method,
        url: &Url,
        body: Option<&serde_json::Value>,
        accept: &str,
        cancel: &CancellationToken,
        classify: impl Fn(Result<Response>) -> Outcome<T>,
    ) -> Result<T> {
        let mut attempt = 0u32;
        let mut refreshed = false;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let token = self.tokens.access_token().await?;
            let mut req = http
                .request(method.clone(), url.as_str())
                .bearer_auth(&token)
                .header(ACCEPT, accept);
            if let Some(b) = body {
                req = req
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(serde_json::to_vec(b)?);
            }
            let outcome = classify(http.send(req, &[]).await);
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
                // A server hint longer than the policy allows inline goes to the supervisor.
                Outcome::Retry(e) => {
                    let Some(delay) = self.retry.pause(attempt, &e) else {
                        return Err(e);
                    };
                    tracing::debug!(%url, attempt, ?delay, "google api: retrying after {e}");
                    attempt += 1;
                    retry::sleep(delay, cancel).await?;
                }
                Outcome::Fatal(e) => return Err(e),
            }
        }
    }

    /// Parse a JSON body, reporting malformed input as `Invalid` (never panics).
    pub(crate) fn json<T: for<'de> Deserialize<'de>>(resp: &Response, what: &str) -> Result<T> {
        serde_json::from_slice(&resp.body)
            .map_err(|e| Error::Invalid(format!("malformed {what} response: {e}")))
    }
}

/// [`classify`] for [`GoogleApi::get_media`]: per-request refusals become
/// [`Media::Refused`] instead of errors.
fn classify_media(resp: Response) -> Outcome<Media> {
    let status = resp.status.as_u16();
    if status == 400 || status == 403 {
        let (reasons, message) = parse_error(&resp.body);
        let limited = reasons
            .iter()
            .any(|r| RATE_REASONS.contains(&r.as_str()) || QUOTA_REASONS.contains(&r.as_str()));
        if status == 400 || !limited {
            let reason = reasons
                .iter()
                .find(|r| !r.is_empty())
                .map(|r| {
                    r.chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .take(64)
                        .collect()
                })
                .unwrap_or_default();
            return Outcome::Done(Media::Refused {
                status,
                reason,
                message,
            });
        }
    }
    match classify(resp) {
        Outcome::Done(resp) => Outcome::Done(Media::Body(resp)),
        Outcome::Refresh => Outcome::Refresh,
        Outcome::Retry(e) => Outcome::Retry(e),
        Outcome::Fatal(e) => Outcome::Fatal(e),
    }
}

fn classify(resp: Response) -> Outcome {
    let status = resp.status.as_u16();
    if resp.status.is_success() || status == 404 || status == 410 {
        return Outcome::Done(resp);
    }
    let retry_after_ms = retry::retry_after_ms(&resp.headers);
    let (reasons, message) = parse_error(&resp.body);
    let has = |set: &[&str]| reasons.iter().any(|r| set.contains(&r.as_str()));
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
