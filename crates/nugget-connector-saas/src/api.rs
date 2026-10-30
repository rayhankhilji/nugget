//! Authenticated, metered, rate-limit-aware requests shared by the four connectors.

use std::sync::Arc;
use std::time::Duration;

use nugget_connector::CancellationToken;
use nugget_connector::http::{MeteredHttp, Response};
use nugget_connector::retry::{self, Backoff};
use nugget_core::{Error, Result};
use reqwest::header::{DATE, HeaderMap};

use crate::RetryPolicy;
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
    /// Exponential with jitter ([`Backoff`]); a `Retry-After` hint is a floor.
    fn backoff(&self, attempt: u32, err: &Error) -> Option<Duration> {
        let hinted = match err {
            Error::RateLimited {
                retry_after_ms: Some(ms),
            } => Some(Duration::from_millis(*ms)),
            _ => None,
        };
        if hinted.is_some_and(|h| h > self.retry.max_delay) {
            return None;
        }
        let b = Backoff {
            base: self.retry.base_delay,
            max: self.retry.max_delay,
        };
        Some(b.delay(attempt, hinted))
    }
}

/// `Retry-After` in milliseconds: delta-seconds or an HTTP-date, the latter measured against
/// the response's own `Date` header (clock-skew safe).
pub(crate) fn retry_after_ms(h: &HeaderMap) -> Option<u64> {
    retry::retry_after_ms(h)
}

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

#[cfg(test)]
mod unit {
    use super::*;
    use nugget_connector::testkit::VecEgress;

    fn api(base_ms: u64, max_ms: u64) -> Api {
        let egress = Arc::new(VecEgress::default());
        let secrets = Arc::new(nugget_connector::testkit::MemSecrets::default());
        Api::new(
            metered(egress, "sync:test").unwrap(),
            Arc::new(Credential::new("Test", secrets, "s", None, None)),
            RetryPolicy {
                max_retries: 8,
                base_delay: Duration::from_millis(base_ms),
                max_delay: Duration::from_millis(max_ms),
            },
            "Test",
        )
    }

    /// Exponential growth, a cap, up to 25% jitter, and a server hint as a floor; a hint over
    /// the cap is not slept on (the pass returns `RateLimited`).
    #[test]
    fn backoff_is_exponential_with_jitter_and_honours_hints() {
        let a = api(100, 2_000);
        let net = Error::Network("x".into());
        for attempt in 0..10u32 {
            let exp = Duration::from_millis(100 * (1u64 << attempt)).min(Duration::from_secs(2));
            let d = a.backoff(attempt, &net).unwrap();
            assert!(d >= exp && d <= exp + exp / 4, "attempt {attempt}: {d:?}");
        }
        let hinted = Error::RateLimited {
            retry_after_ms: Some(1_500),
        };
        assert!(a.backoff(0, &hinted).unwrap() >= Duration::from_millis(1_500));
        let too_long = Error::RateLimited {
            retry_after_ms: Some(60_000),
        };
        assert_eq!(a.backoff(0, &too_long), None);
    }

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
