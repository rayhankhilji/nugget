//! Authenticated, metered, throttling-aware GET requests against Microsoft Graph.

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

use crate::settings::Common;
use crate::{MicrosoftEndpoints, RetryPolicy};

/// Error codes Graph uses for an expired or invalid delta token: start the listing over.
const RESYNC_CODES: &[&str] = &[
    "syncstatenotfound",
    "syncstateinvalid",
    "resyncrequired",
    "resyncchangesapplydifferences",
    "resyncchangesuploaddifferences",
    "resyncapplydifferences",
    "resyncuploaddifferences",
];

/// Longest JSON page accepted from Graph.
const MAX_JSON: usize = 32 * 1024 * 1024;

pub(crate) struct GraphApi {
    http: MeteredHttp,
    tokens: Arc<TokenSource>,
    retry: RetryPolicy,
    ctx: ConnectorContext,
    login_hint: Option<String>,
    base: Url,
}

enum Outcome {
    /// 2xx, 404, 410 and resync-coded 400s (and 3xx when asked for): the caller decides.
    Done(Response),
    /// 401: refresh the token once and retry.
    Refresh,
    /// Worth retrying after a backoff.
    Retry(Error),
    Fatal(Error),
}

#[derive(Deserialize, Default)]
struct GraphError {
    #[serde(default)]
    error: GraphErrorBody,
}

#[derive(Deserialize, Default)]
struct GraphErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// Options for one GET.
#[derive(Default, Clone, Copy)]
pub(crate) struct Get<'a> {
    /// `Prefer` header values.
    pub prefer: &'a [&'a str],
    /// Return 3xx responses (their `Location` is inspected, never followed).
    pub allow_redirect: bool,
    /// Response body cap; 0 = [`MAX_JSON`].
    pub max_body: usize,
    /// Not JSON (e.g. MIME from `$value`): `Accept: */*`.
    pub raw: bool,
}

impl GraphApi {
    pub(crate) fn new(
        feature: &str,
        scopes: &[&str],
        common: &Common,
        endpoints: &MicrosoftEndpoints,
        retry: RetryPolicy,
        ctx: ConnectorContext,
    ) -> Result<Self> {
        let http = MeteredHttp::new(ctx.egress.clone(), feature)?;
        let provider = crate::oauth::provider_with_login(&endpoints.login, &common.tenant, scopes)?;
        let creds = ClientCredentials::new(common.client_id.trim(), common.client_secret.clone());
        let client = OAuthClient::new(provider, creds, http.clone())?;
        let tokens = TokenSource::new(
            client,
            ctx.secrets.clone(),
            oauth_account(ctx.source.as_str()),
        );
        let base = Url::parse(&endpoints.graph_api).map_err(|e| {
            Error::Invalid(format!("bad Graph base `{}`: {e}", endpoints.graph_api))
        })?;
        if base.cannot_be_a_base() || !matches!(base.scheme(), "https" | "http") {
            return Err(Error::Invalid("Graph base must be an http(s) URL".into()));
        }
        Ok(Self {
            http,
            tokens: Arc::new(tokens),
            retry,
            ctx,
            login_hint: common.account_hint.clone().filter(|h| !h.trim().is_empty()),
            base,
        })
    }

    pub(crate) fn retry(&self) -> RetryPolicy {
        self.retry
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
                self.ctx.secrets.clone(),
                oauth_account(self.ctx.source.as_str()),
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

    /// Fail fast with `Unauthorized` when no tokens are stored.
    pub(crate) async fn ensure_token(&self) -> Result<()> {
        self.tokens.access_token().await.map(|_| ())
    }

    /// `<base>/<segments...>?<query>`, with each segment percent-encoded.
    /// `.`, `..` and empty segments are refused: `url` would drop or keep them silently, so an
    /// id from a response could address another endpoint than the one meant.
    pub(crate) fn url(&self, segments: &[&str], query: &[(&str, &str)]) -> Result<Url> {
        if segments
            .iter()
            .any(|s| s.is_empty() || *s == "." || *s == "..")
        {
            return Err(Error::Invalid("bad Graph path segment".into()));
        }
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| Error::Invalid("bad Graph base".into()))?
            .pop_if_empty()
            .extend(segments);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    /// Validate an `@odata.nextLink` / `@odata.deltaLink` (or one read back from a cursor): it
    /// must stay on the Graph origin under the API root, so a hostile payload cannot send the
    /// bearer token elsewhere.
    pub(crate) fn link(&self, link: &str) -> Result<Url> {
        let bad = || {
            Error::Invalid(format!(
                "Graph returned a link outside the API: `{}`",
                link.chars().take(120).collect::<String>()
            ))
        };
        if link.len() > 16 * 1024 {
            return Err(bad());
        }
        let url = Url::parse(link).map_err(|_| bad())?;
        let root = self.base.path().trim_end_matches('/');
        let path_ok = url.path() == root || url.path().starts_with(&format!("{root}/"));
        if url.origin() != self.base.origin()
            || !path_ok
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(bad());
        }
        Ok(url)
    }

    /// GET `url` as JSON with the bearer token. Returns 2xx, 404, 410 and resync-coded 400
    /// responses (and 3xx with `allow_redirect`); maps the rest to typed errors. 401 refreshes
    /// the token once; 429, 503/504, 408 and 5xx are retried with exponential backoff
    /// (honouring `Retry-After`) up to the policy's limit.
    pub(crate) async fn get(
        &self,
        url: &Url,
        opts: Get<'_>,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        let mut attempt = 0u32;
        let mut refreshed = false;
        let http = self.http.clone().with_max_body(if opts.max_body == 0 {
            MAX_JSON
        } else {
            opts.max_body
        });
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let token = self.tokens.access_token().await?;
            let mut req = http
                .request(Method::GET, url.as_str())
                .bearer_auth(&token)
                .header(ACCEPT, if opts.raw { "*/*" } else { "application/json" });
            for p in opts.prefer {
                req = req.header("Prefer", *p);
            }
            let outcome = match http.send(req, &[]).await {
                Ok(resp) => classify(resp, opts.allow_redirect),
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
                        "Microsoft rejected the access token; reconnect the account".into(),
                    ));
                }
                Outcome::Retry(e) if attempt < self.retry.max_retries => {
                    let delay = backoff(&self.retry, attempt, &e);
                    tracing::debug!(
                        path = url.path(),
                        attempt,
                        ?delay,
                        "graph: retrying after {e}"
                    );
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

    /// Parse a JSON body, reporting malformed input as `Invalid` (never panics).
    pub(crate) fn json<T: for<'de> Deserialize<'de>>(resp: &Response, what: &str) -> Result<T> {
        serde_json::from_slice(&resp.body)
            .map_err(|e| Error::Invalid(format!("malformed {what} response: {e}")))
    }
}

/// Whether a response says the delta token is gone and the listing must start over.
pub(crate) fn needs_resync(resp: &Response) -> bool {
    match resp.status.as_u16() {
        410 => true,
        400 | 404 => {
            let e: GraphError = serde_json::from_slice(&resp.body).unwrap_or_default();
            RESYNC_CODES.contains(&e.error.code.to_ascii_lowercase().as_str())
        }
        _ => false,
    }
}

/// An unexpected status as an error, with Graph's error code (never the raw body).
pub(crate) fn status_error(what: &str, resp: &Response) -> Error {
    let e: GraphError = serde_json::from_slice(&resp.body).unwrap_or_default();
    let code: String = e.error.code.chars().take(80).collect();
    let status = resp.status.as_u16();
    let msg = format!("{what}: HTTP {status} {code}");
    match status {
        404 => Error::NotFound(msg),
        _ => Error::Other(msg),
    }
}

pub(crate) fn retry_after_ms(resp: &Response) -> Option<u64> {
    resp.headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|s| s.min(86_400).saturating_mul(1000))
}

pub(crate) fn backoff(retry: &RetryPolicy, attempt: u32, err: &Error) -> Duration {
    let exp = retry.base_delay.saturating_mul(1u32 << attempt.min(16));
    let hinted = match err {
        Error::RateLimited {
            retry_after_ms: Some(ms),
        } => Duration::from_millis(*ms),
        _ => Duration::ZERO,
    };
    let base = exp.max(hinted).min(retry.max_delay);
    // Up to 25% jitter so parallel sources do not retry in lockstep.
    let nanos = time::OffsetDateTime::now_utc().nanosecond() as u64;
    let jitter = base.as_millis() as u64 / 4;
    base + Duration::from_millis(if jitter == 0 { 0 } else { nanos % jitter })
}

fn classify(resp: Response, allow_redirect: bool) -> Outcome {
    let status = resp.status.as_u16();
    if resp.status.is_success() || status == 404 || needs_resync(&resp) {
        return Outcome::Done(resp);
    }
    if resp.status.is_redirection() {
        return if allow_redirect {
            Outcome::Done(resp)
        } else {
            Outcome::Fatal(Error::Network(format!(
                "unexpected redirect from Microsoft Graph (HTTP {status})"
            )))
        };
    }
    let retry_after = retry_after_ms(&resp);
    let body: GraphError = serde_json::from_slice(&resp.body).unwrap_or_default();
    let code: String = body.error.code.chars().take(80).collect();
    let message: String = body.error.message.chars().take(300).collect();
    match status {
        401 => Outcome::Refresh,
        429 | 509 => Outcome::Retry(Error::RateLimited {
            retry_after_ms: retry_after,
        }),
        503 | 504 if retry_after.is_some() => Outcome::Retry(Error::RateLimited {
            retry_after_ms: retry_after,
        }),
        403 => Outcome::Fatal(Error::Unauthorized(format!(
            "Microsoft Graph denied access (HTTP 403 {code}): {message}"
        ))),
        408 | 500..=599 => Outcome::Retry(Error::Network(format!(
            "Microsoft Graph HTTP {status} {code}: {message}"
        ))),
        400 => Outcome::Fatal(Error::Invalid(format!(
            "Microsoft Graph rejected the request ({code}): {message}"
        ))),
        _ => Outcome::Fatal(Error::Other(format!(
            "Microsoft Graph HTTP {status} {code}: {message}"
        ))),
    }
}

/// One page of a delta query. Items stay raw JSON so one malformed item is skipped instead of
/// failing the page.
#[derive(Deserialize)]
pub(crate) struct DeltaPage {
    #[serde(default)]
    pub value: Vec<serde_json::Value>,
    #[serde(default, rename = "@odata.nextLink")]
    pub next_link: Option<String>,
    #[serde(default, rename = "@odata.deltaLink")]
    pub delta_link: Option<String>,
}

/// Whether a delta item is an `@removed` tombstone.
pub(crate) fn is_removed(item: &serde_json::Value) -> bool {
    item.get("@removed").is_some()
}
