//! Metered HTTP for connectors: every exchange is recorded in the egress ledger, redirects are
//! never followed, and response bodies are size-capped.

use std::sync::Arc;
use std::time::Duration;

use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::ItemRef;
use nugget_core::{Error, Result};
use reqwest::{Method, StatusCode, header::HeaderMap};
use time::OffsetDateTime;

/// Default cap on a response body.
pub const DEFAULT_MAX_BODY: usize = 64 * 1024 * 1024;

/// A fully-read HTTP response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    /// `Err` for non-2xx: 401/403 → `Unauthorized`, 429 → `RateLimited`, 5xx → `Network`,
    /// others → `Other`. Any 3xx is reported as an unexpected redirect.
    pub fn error_for_status(self) -> Result<Self> {
        let s = self.status;
        if s.is_success() {
            return Ok(self);
        }
        let detail = String::from_utf8_lossy(&self.body[..self.body.len().min(512)]).into_owned();
        Err(match s.as_u16() {
            300..=399 => Error::Network(format!("unexpected redirect (HTTP {s})")),
            401 | 403 => Error::Unauthorized(format!("HTTP {s}: {detail}")),
            429 => Error::RateLimited {
                retry_after_ms: retry_after_ms(&self.headers),
            },
            500..=599 => Error::Network(format!("HTTP {s}: {detail}")),
            _ => Error::Other(format!("HTTP {s}: {detail}")),
        })
    }
}

fn retry_after_ms(h: &HeaderMap) -> Option<u64> {
    let v = h.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<u64>().ok().map(|s| s.saturating_mul(1000))
}

/// A reqwest client bound to one source's egress ledger.
#[derive(Clone)]
pub struct MeteredHttp {
    client: reqwest::Client,
    egress: Arc<dyn EgressSink>,
    feature: String,
    max_body: usize,
}

impl std::fmt::Debug for MeteredHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeteredHttp")
            .field("feature", &self.feature)
            .finish_non_exhaustive()
    }
}

impl MeteredHttp {
    /// `feature` is recorded on every ledger row, e.g. `sync:gmail`.
    pub fn new(egress: Arc<dyn EgressSink>, feature: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("nugget/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Other(format!("http client: {e}")))?;
        Ok(Self::with_client(client, egress, feature))
    }

    /// Use a pre-built client. It must not follow redirects.
    pub fn with_client(
        client: reqwest::Client,
        egress: Arc<dyn EgressSink>,
        feature: impl Into<String>,
    ) -> Self {
        Self {
            client,
            egress,
            feature: feature.into(),
            max_body: DEFAULT_MAX_BODY,
        }
    }

    pub fn with_max_body(mut self, max: usize) -> Self {
        self.max_body = max;
        self
    }

    /// Start a request (add headers, auth, body), then pass it to [`send`](Self::send).
    pub fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        self.client.request(method, url)
    }

    /// Send a request, read the whole body (up to the cap) and record egress. `items` names the
    /// stored items whose content is included in the request, if any.
    pub async fn send(&self, req: reqwest::RequestBuilder, items: &[ItemRef]) -> Result<Response> {
        let req = req
            .build()
            .map_err(|e| Error::Invalid(format!("bad request: {e}")))?;
        let host = req
            .url()
            .host_str()
            .map(|h| match req.url().port() {
                Some(p) => format!("{h}:{p}"),
                None => h.to_string(),
            })
            .unwrap_or_default();
        let sent = approx_request_bytes(&req);
        let mut received = 0u64;
        let result = self.exchange(req, &mut received).await;
        self.egress.record(EgressEvent {
            at: OffsetDateTime::now_utc(),
            host,
            feature: self.feature.clone(),
            bytes_sent: sent,
            bytes_received: received,
            items: items.to_vec(),
        });
        result
    }

    async fn exchange(&self, req: reqwest::Request, received: &mut u64) -> Result<Response> {
        let mut resp = self.client.execute(req).await.map_err(net_err)?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(net_err)? {
            *received += chunk.len() as u64;
            if body.len() + chunk.len() > self.max_body {
                return Err(Error::Invalid(format!(
                    "response body exceeds {} bytes",
                    self.max_body
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

fn net_err(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Network(format!("timed out: {e}"))
    } else {
        Error::Network(format!("request failed: {e}"))
    }
}

fn approx_request_bytes(req: &reqwest::Request) -> u64 {
    let headers: usize = req
        .headers()
        .iter()
        .map(|(k, v)| k.as_str().len() + v.len() + 4)
        .sum();
    let body = req.body().and_then(|b| b.as_bytes()).map_or(0, <[u8]>::len);
    (req.method().as_str().len() + req.url().as_str().len() + 12 + headers + body) as u64
}
