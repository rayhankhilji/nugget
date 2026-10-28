//! Authenticated, metered, rate-limit-aware requests shared by the four connectors.

use std::sync::Arc;
use std::time::Duration;

use nugget_connector::CancellationToken;
use nugget_connector::http::{MeteredHttp, Response};
use nugget_core::{Error, Result};
use reqwest::header::HeaderMap;

use crate::RetryPolicy;
use crate::auth::{Credential, Token};

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
                Outcome::Retry(e) if attempt < self.retry.max_retries => {
                    let Some(delay) = self.backoff(attempt, &e) else {
                        // The server asked for a longer pause than we sleep inline.
                        return Err(e);
                    };
                    tracing::debug!(
                        service = self.service,
                        attempt,
                        ?delay,
                        "retrying after {e}"
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

    /// The pause before retry `attempt`, or `None` when the server's hint exceeds `max_delay`.
    fn backoff(&self, attempt: u32, err: &Error) -> Option<Duration> {
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
        if hinted > self.retry.max_delay {
            return None;
        }
        let base = exp.max(hinted).min(self.retry.max_delay);
        // Up to 25% jitter so parallel sources do not retry in lockstep.
        let nanos = u64::from(time::OffsetDateTime::now_utc().nanosecond());
        let jitter = base.as_millis() as u64 / 4;
        Some(base + Duration::from_millis(if jitter == 0 { 0 } else { nanos % jitter }))
    }
}

/// `Retry-After` in milliseconds (delta seconds only; HTTP dates are ignored).
pub(crate) fn retry_after_ms(h: &HeaderMap) -> Option<u64> {
    header_u64(h, "retry-after").map(|s| s.saturating_mul(1000))
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
