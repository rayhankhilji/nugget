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
    /// Worth retrying after a backoff; `true` when the server answered (not a transport error).
    Retry(Error, bool),
    /// Item requests only: this item cannot be read now (skip it, retry on a later pass).
    ItemFailed(String),
    Fatal(Error),
}

/// Result of [`GraphApi::get_item`].
pub(crate) enum ItemGet {
    Done(Response),
    /// The body is over the cap.
    TooLarge,
    /// Graph failed or refused this one item; the reason is for logs.
    Failed(String),
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
    /// the token once; 429, 503/504, 408, 409, 423 and 5xx are retried with exponential backoff
    /// (honouring `Retry-After`) up to the policy's limit.
    pub(crate) async fn get(
        &self,
        url: &Url,
        opts: Get<'_>,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        match self.request(url, opts, cancel, false).await? {
            ItemGet::Done(r) => Ok(r),
            ItemGet::TooLarge => Err(Error::Invalid(format!(
                "Graph response body exceeds {} bytes",
                if opts.max_body == 0 {
                    MAX_JSON
                } else {
                    opts.max_body
                }
            ))),
            ItemGet::Failed(m) => Err(Error::Other(m)),
        }
    }

    /// GET one item's content (a message's MIME, a file's bytes). Like [`get`](Self::get), but
    /// a failure that is about this item alone — Graph answered with a 5xx/408/409/423 after
    /// the retries, a 400, or a 403 `accessDenied` for the item — is [`ItemGet::Failed`], and a
    /// body over the cap is [`ItemGet::TooLarge`]. Everything else still ends the pass as an
    /// `Err`: token and keychain failures, a rejected token, other 403s, throttling once the
    /// budget is spent, and transport failures (the network is down).
    pub(crate) async fn get_item(
        &self,
        url: &Url,
        opts: Get<'_>,
        cancel: &CancellationToken,
    ) -> Result<ItemGet> {
        self.request(url, opts, cancel, true).await
    }

    async fn request(
        &self,
        url: &Url,
        opts: Get<'_>,
        cancel: &CancellationToken,
        item: bool,
    ) -> Result<ItemGet> {
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
            // Token errors always end the pass, whatever their kind.
            let token = self.tokens.access_token().await?;
            let mut req = http
                .request(Method::GET, url.as_str())
                .bearer_auth(&token)
                .header(ACCEPT, if opts.raw { "*/*" } else { "application/json" });
            for p in opts.prefer {
                req = req.header("Prefer", *p);
            }
            let outcome = match http.send(req, &[]).await {
                Ok(resp) => classify(resp, opts.allow_redirect, item),
                Err(Error::Invalid(m)) if m.contains("exceeds") => return Ok(ItemGet::TooLarge),
                Err(e @ Error::Network(_)) => Outcome::Retry(e, false),
                Err(e) => Outcome::Fatal(e),
            };
            match outcome {
                Outcome::Done(resp) => return Ok(ItemGet::Done(resp)),
                Outcome::ItemFailed(m) => return Ok(ItemGet::Failed(m)),
                Outcome::Refresh if !refreshed => {
                    refreshed = true;
                    self.tokens.force_refresh(&token).await?;
                }
                Outcome::Refresh => {
                    return Err(Error::Unauthorized(
                        "Microsoft rejected the access token; reconnect the account".into(),
                    ));
                }
                Outcome::Retry(e, server) => {
                    let delay = (attempt < self.retry.max_retries)
                        .then(|| backoff(&self.retry, attempt, &e))
                        .flatten();
                    let Some(delay) = delay else {
                        // Out of retries (or asked to wait longer than a pass should).
                        return match e {
                            Error::Network(m) if item && server => Ok(ItemGet::Failed(m)),
                            e => Err(e),
                        };
                    };
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

/// `Retry-After` in milliseconds: delta-seconds or an HTTP-date, the latter measured against
/// the response's `Date` header so a skewed local clock does not distort it.
pub(crate) fn retry_after_ms(resp: &Response) -> Option<u64> {
    nugget_connector::retry::retry_after_ms(&resp.headers)
}

/// The wait before retry number `attempt` (0-based): `base * 2^attempt` capped at `max_delay`,
/// plus up to 25% jitter, never shorter than a server-requested `Retry-After`. `None` when the
/// server asks for a longer wait than `max_delay`: the error is returned so the supervisor
/// waits instead of retrying early.
pub(crate) fn backoff(retry: &RetryPolicy, attempt: u32, err: &Error) -> Option<Duration> {
    let hinted = match err {
        Error::RateLimited {
            retry_after_ms: Some(ms),
        } => Duration::from_millis(*ms),
        _ => Duration::ZERO,
    };
    if hinted > retry.max_delay {
        return None;
    }
    let exp = retry
        .base_delay
        .saturating_mul(1u32 << attempt.min(16))
        .min(retry.max_delay);
    // Up to 25% jitter so parallel sources do not retry in lockstep.
    let nanos = time::OffsetDateTime::now_utc().nanosecond() as u64;
    let jitter = exp.as_millis() as u64 / 4;
    let d = exp + Duration::from_millis(if jitter == 0 { 0 } else { nanos % jitter });
    Some(d.max(hinted))
}

/// Graph error codes of a 403 that concern one item (blocked or protected content), not the
/// account's access.
const ITEM_DENIED_CODES: &[&str] = &["accessdenied", "erroraccessdenied"];

fn classify(resp: Response, allow_redirect: bool, item: bool) -> Outcome {
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
        429 | 509 => Outcome::Retry(
            Error::RateLimited {
                retry_after_ms: retry_after,
            },
            true,
        ),
        503 | 504 if retry_after.is_some() => Outcome::Retry(
            Error::RateLimited {
                retry_after_ms: retry_after,
            },
            true,
        ),
        403 if item && ITEM_DENIED_CODES.contains(&code.to_ascii_lowercase().as_str()) => {
            Outcome::ItemFailed(format!("Microsoft Graph HTTP 403 {code}: {message}"))
        }
        403 => Outcome::Fatal(Error::Unauthorized(format!(
            "Microsoft Graph denied access (HTTP 403 {code}): {message}"
        ))),
        408 | 409 | 423 | 500..=599 => Outcome::Retry(
            Error::Network(format!("Microsoft Graph HTTP {status} {code}: {message}")),
            true,
        ),
        400 if item => Outcome::ItemFailed(format!("Microsoft Graph HTTP 400 {code}: {message}")),
        400 => Outcome::Fatal(Error::Invalid(format!(
            "Microsoft Graph rejected the request ({code}): {message}"
        ))),
        _ if item => {
            Outcome::ItemFailed(format!("Microsoft Graph HTTP {status} {code}: {message}"))
        }
        _ => Outcome::Fatal(Error::Other(format!(
            "Microsoft Graph HTTP {status} {code}: {message}"
        ))),
    }
}

/// Items carried in a cursor for another try.
pub(crate) const MAX_RETRY_ITEMS: usize = 1_000;
/// Tries before an item that keeps failing is given up (with a warning).
pub(crate) const MAX_ITEM_ATTEMPTS: u32 = 5;
/// Item fetches failing in a row that mean an outage, not bad items: the pass ends.
pub(crate) const MAX_FAILURE_STREAK: u32 = 5;

/// An item whose content could not be fetched, kept in the cursor and fetched again on later
/// passes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
pub(crate) struct RetryItem {
    pub key: String,
    pub attempts: u32,
}

/// Bookkeeping of item failures in one pass.
pub(crate) struct Skips {
    /// To carry in the cursor.
    pub list: Vec<RetryItem>,
    /// Attempts made before this pass.
    prior: std::collections::HashMap<String, u32>,
    /// Handled (fetched, failed again, resolved) in this pass.
    tried: std::collections::HashSet<String>,
    streak: u32,
}

impl Skips {
    pub(crate) fn new(mut carried: Vec<RetryItem>) -> Self {
        carried.truncate(MAX_RETRY_ITEMS);
        Self {
            prior: carried
                .iter()
                .map(|r| (r.key.clone(), r.attempts))
                .collect(),
            list: carried,
            tried: Default::default(),
            streak: 0,
        }
    }

    /// The item was fetched (or is gone, unchanged or out of scope): no retry needed.
    pub(crate) fn resolved(&mut self, key: &str) {
        self.tried.insert(key.to_string());
        self.list.retain(|r| r.key != key);
    }

    /// An item fetch succeeded: the failure streak is over.
    pub(crate) fn fetched(&mut self, key: &str) {
        self.streak = 0;
        self.resolved(key);
    }

    /// An item fetch failed: keep it for a later pass (until [`MAX_ITEM_ATTEMPTS`]). Fails the
    /// pass after [`MAX_FAILURE_STREAK`] failures in a row, so an outage is not taken for bad
    /// items.
    pub(crate) fn failed(&mut self, source: &str, key: &str, reason: &str) -> Result<()> {
        self.streak += 1;
        if self.streak >= MAX_FAILURE_STREAK {
            return Err(Error::Network(format!(
                "{} item downloads failed in a row; last: {reason}",
                self.streak
            )));
        }
        let attempts = self.prior.get(key).copied().unwrap_or(0) + 1;
        self.tried.insert(key.to_string());
        self.list.retain(|r| r.key != key);
        if attempts >= MAX_ITEM_ATTEMPTS {
            tracing::warn!(
                source,
                item = key,
                "giving up on an item after {attempts} failed fetches: {reason}"
            );
        } else if self.list.len() < MAX_RETRY_ITEMS {
            tracing::warn!(
                source,
                item = key,
                "skipping an unreadable item for now: {reason}"
            );
            self.list.push(RetryItem {
                key: key.to_string(),
                attempts,
            });
        }
        Ok(())
    }

    /// Carried items not handled yet in this pass.
    pub(crate) fn pending(&self) -> Vec<String> {
        self.list
            .iter()
            .filter(|r| !self.tried.contains(&r.key))
            .map(|r| r.key.clone())
            .collect()
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
