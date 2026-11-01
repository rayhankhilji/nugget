//! Authenticated, metered, rate-limit-aware requests shared by the four connectors.

use std::sync::Arc;

use nugget_connector::CancellationToken;
use nugget_connector::http::{MeteredHttp, Response};
use nugget_connector::retry::{self, RetryPolicy};
use nugget_core::{Error, Result};
use reqwest::header::{DATE, HeaderMap};
use serde_json::Value;

use crate::auth::{Credential, Token};
use crate::text::now_unix;

/// Largest response body read from any of the APIs (hostile-input cap).
pub(crate) const MAX_BODY: usize = 16 * 1024 * 1024;

/// What to do with a response.
pub(crate) enum Outcome {
    /// Hand the response to the caller (2xx, and statuses the caller interprets itself).
    Done(Response),
    /// 401: refresh the token once (OAuth) and retry, else `Unauthorized`.
    Refresh,
    /// Worth retrying after a backoff (`RateLimited` hints are honoured).
    Retry(Error),
    Fatal(Error),
}

/// One service's HTTP client: credential, egress-metered transport and retry policy.
pub(crate) struct Api {
    pub(crate) http: MeteredHttp,
    pub(crate) cred: Arc<Credential>,
    retry: RetryPolicy,
    service: &'static str,
}

impl Api {
    pub(crate) fn new(
        http: MeteredHttp,
        cred: Arc<Credential>,
        retry: RetryPolicy,
        service: &'static str,
    ) -> Self {
        Self {
            http,
            cred,
            retry,
            service,
        }
    }

    /// Send the request `build` makes (called again for each attempt with the current token),
    /// classify the response with `classify`, and retry or refresh as it says.
    pub(crate) async fn execute<B>(
        &self,
        cancel: &CancellationToken,
        classify: fn(Response) -> Outcome,
        build: B,
    ) -> Result<Response>
    where
        B: Fn(&MeteredHttp, &Token) -> reqwest::RequestBuilder,
    {
        let mut attempt = 0u32;
        let mut refreshed = false;
        let mut token = self.cred.token().await?;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let req = build(&self.http, &token);
            let sent = tokio::select! {
                r = self.http.send(req, &[]) => r,
                _ = cancel.cancelled() => return Err(Error::Cancelled),
            };
            let outcome = match sent {
                Ok(resp) => classify(resp),
                Err(e @ Error::Network(_)) => Outcome::Retry(e),
                Err(e) => Outcome::Fatal(e),
            };
            match outcome {
                Outcome::Done(resp) => return Ok(resp),
                Outcome::Refresh if !refreshed => {
                    refreshed = true;
                    token = self.cred.refresh(&token).await?;
                }
                Outcome::Refresh => {
                    return Err(Error::Unauthorized(format!(
                        "{} rejected the token; reconnect the source",
                        self.service
                    )));
                }
                Outcome::Retry(e) => {
                    // `None`: retries spent, or the server asked for a longer pause than we
                    // sleep inline.
                    let Some(delay) = self.retry.pause(attempt, &e) else {
                        return Err(e);
                    };
                    tracing::debug!(
                        service = self.service,
                        attempt,
                        ?delay,
                        "retrying after {e}"
                    );
                    attempt += 1;
                    retry::sleep(delay, cancel).await?;
                }
                Outcome::Fatal(e) => return Err(e),
            }
        }
    }
}

pub(crate) use retry::retry_after_ms;

/// The server's clock (its `Date` header) in Unix seconds, else the local clock. Reset times
/// the server sends (`x-ratelimit-reset`, `resetAt`) are measured against it, so a local clock
/// that is off does not turn "wait 2 s" into "wait an hour" or "retry now".
pub(crate) fn server_now(h: &HeaderMap) -> i64 {
    h.get(DATE)
        .and_then(|v| v.to_str().ok())
        .and_then(retry::parse_http_date)
        .map_or_else(now_unix, |t| t.unix_timestamp())
}

/// Whether `e` concerns reading one item (its content, comments, replies or blocks) rather
/// than the whole pass: such an item is skipped and retried later instead of failing the sync.
/// Credentials, rate limits and cancellation always stop the pass.
pub(crate) fn item_error(e: &Error) -> bool {
    matches!(
        e,
        Error::Network(_) | Error::Other(_) | Error::Invalid(_) | Error::NotFound(_)
    )
}

/// Consecutive per-item failures in one pass. A few bad items are skipped; a run of failures
/// means the service (not the item) is failing, and the error surfaces.
#[derive(Default)]
pub(crate) struct ItemFailures {
    run: usize,
}

impl ItemFailures {
    pub(crate) const MAX_IN_A_ROW: usize = 5;

    pub(crate) fn ok(&mut self) {
        self.run = 0;
    }

    /// `Ok` when the item may be skipped, else the error to return.
    pub(crate) fn skip(&mut self, e: Error, service: &str, what: &str) -> Result<()> {
        if !item_error(&e) {
            return Err(e);
        }
        self.run += 1;
        if self.run >= Self::MAX_IN_A_ROW {
            return Err(e);
        }
        tracing::warn!(
            service,
            item = what,
            "skipping an item that failed to load: {e}"
        );
        Ok(())
    }
}

pub(crate) fn header_u64(h: &HeaderMap, name: &str) -> Option<u64> {
    h.get(name)?.to_str().ok()?.trim().parse::<u64>().ok()
}

/// The classification every service shares for statuses it does not treat specially.
pub(crate) fn classify_common(resp: Response, service: &str) -> Outcome {
    let status = resp.status.as_u16();
    if resp.status.is_success() {
        return Outcome::Done(resp);
    }
    let retry_after_ms = retry_after_ms(&resp.headers);
    match status {
        401 => Outcome::Refresh,
        429 => Outcome::Retry(Error::RateLimited { retry_after_ms }),
        403 => Outcome::Fatal(Error::Unauthorized(format!(
            "{service} denied access (HTTP 403): {}",
            snippet(&resp.body)
        ))),
        300..=399 => Outcome::Fatal(Error::Network(format!(
            "unexpected redirect from {service} (HTTP {status})"
        ))),
        408 | 409 | 500..=599 => Outcome::Retry(Error::Network(format!(
            "{service} HTTP {status}: {}",
            snippet(&resp.body)
        ))),
        400 => Outcome::Fatal(Error::Invalid(format!(
            "{service} rejected the request: {}",
            snippet(&resp.body)
        ))),
        _ => Outcome::Fatal(Error::Other(format!(
            "{service} HTTP {status}: {}",
            snippet(&resp.body)
        ))),
    }
}

/// A short, printable excerpt of an error body for messages.
pub(crate) fn snippet(body: &[u8]) -> String {
    String::from_utf8_lossy(&body[..body.len().min(300)])
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// Parse a JSON body, reporting malformed input as `Invalid` (never panics).
pub(crate) fn json<T: for<'de> serde::Deserialize<'de>>(resp: &Response, what: &str) -> Result<T> {
    serde_json::from_slice(&resp.body)
        .map_err(|e| Error::Invalid(format!("malformed {what} response: {e}")))
}

/// A metered client for `feature` with the crate's body cap.
pub(crate) fn metered(
    egress: Arc<dyn nugget_core::egress::EgressSink>,
    feature: &str,
) -> Result<MeteredHttp> {
    Ok(MeteredHttp::new(egress, feature)?.with_max_body(MAX_BODY))
}

/// Passes an item may fail in a row before the connector stops holding its watermark for it
/// (and stops retrying it): a permanently broken item must not pin the cursor forever. The
/// periodic full listing still covers it.
pub(crate) const MAX_ITEM_ATTEMPTS: u32 = 5;
/// Failing items remembered in a cursor, at most.
pub(crate) const MAX_TRACKED_FAILURES: usize = 200;

/// Record that `key` failed to load in this pass (`failed` is the cursor's per-item count of
/// failed passes). `true`: hold the watermark at the item so the next pass retries it.
/// `false`: give up on it (too many attempts, or too many failing items tracked).
pub(crate) fn note_item_failure(
    failed: &mut std::collections::BTreeMap<String, u32>,
    key: &str,
    service: &str,
) -> bool {
    let n = failed.get(key).copied().unwrap_or(0) + 1;
    if n >= MAX_ITEM_ATTEMPTS {
        failed.remove(key);
        tracing::warn!(
            service,
            item = key,
            attempts = n,
            "giving up on an item that keeps failing to load"
        );
        return false;
    }
    if !failed.contains_key(key) && failed.len() >= MAX_TRACKED_FAILURES {
        tracing::warn!(
            service,
            item = key,
            "too many failing items; not retrying this one"
        );
        return false;
    }
    failed.insert(key.to_string(), n);
    true
}

/// The first error of a GraphQL answer that carries no data (`None` for full or partial data),
/// with its message cut to one short line.
pub(crate) fn graphql_failure(v: &Value) -> Option<(&Value, String)> {
    let errors = v["errors"].as_array().filter(|e| !e.is_empty())?;
    if v["data"]
        .as_object()
        .is_some_and(|d| !d.values().all(Value::is_null))
    {
        return None;
    }
    let msg = errors[0]["message"]
        .as_str()
        .map(|m| crate::text::one_line(m, 200));
    Some((&errors[0], msg.unwrap_or_default()))
}

/// The comments of a GraphQL `node` past the page it embedded, fetched with `page(id, after)`
/// (which answers with the comments connection), until `max` comments in all.
pub(crate) async fn more_comments<F, Fut>(
    node: &Value,
    max: usize,
    mut page: F,
) -> Result<Vec<Value>>
where
    F: FnMut(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let mut out = Vec::new();
    let Some(id) = node["id"].as_str() else {
        return Ok(out);
    };
    let mut after = node["comments"]["pageInfo"]["endCursor"]
        .as_str()
        .map(str::to_string);
    let mut fetched = node["comments"]["nodes"].as_array().map_or(0, Vec::len);
    while let Some(a) = after.take() {
        if fetched >= max {
            break;
        }
        let c = page(id.to_string(), a).await?;
        let nodes = c["nodes"].as_array().cloned().unwrap_or_default();
        if nodes.is_empty() {
            break;
        }
        fetched += nodes.len();
        out.extend(nodes);
        if c["pageInfo"]["hasNextPage"].as_bool() == Some(true) {
            after = c["pageInfo"]["endCursor"].as_str().map(str::to_string);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn only_item_errors_are_skippable_and_a_run_of_them_surfaces() {
        let mut f = ItemFailures::default();
        assert!(f.skip(Error::Unauthorized("x".into()), "T", "i").is_err());
        assert!(
            f.skip(
                Error::RateLimited {
                    retry_after_ms: None
                },
                "T",
                "i"
            )
            .is_err()
        );
        assert!(f.skip(Error::Cancelled, "T", "i").is_err());
        for _ in 0..ItemFailures::MAX_IN_A_ROW - 1 {
            f.skip(Error::Network("x".into()), "T", "i").unwrap();
        }
        assert!(f.skip(Error::Network("x".into()), "T", "i").is_err());
        f.ok();
        f.skip(Error::Other("x".into()), "T", "i").unwrap();
    }
}
