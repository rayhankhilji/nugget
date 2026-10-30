//! Second-stage reranking with a cross-encoder.
//!
//! A [`Reranker`] scores `(query, passage)` pairs jointly, which separates relevant from merely
//! similar passages better than the first-stage scores (bm25, cosine, rank fusion) can.
//! [`Retriever::with_reranker`](crate::Retriever::with_reranker) puts one in the pipeline; any
//! error or timeout keeps the first-stage order (see
//! [`RetrieveConfig::rerank_pool`](crate::RetrieveConfig::rerank_pool)).
//!
//! [`HttpReranker`] calls a local cross-encoder behind llama-server's rerank endpoint
//! (`llama-server --reranking` with a reranker GGUF, e.g. one served by `nugget-runtime`), which
//! speaks the Jina/Cohere shape:
//!
//! ```text
//! POST {base}/v1/rerank   {"model": "...", "query": "...", "documents": ["...", ...], "top_n": N}
//! 200                     {"results": [{"index": 0, "relevance_score": 3.2}, ...]}
//! ```
//!
//! Passages are the user's content, so the endpoint must be local: a base URL on this device or
//! the LAN (judged from the URL alone: `localhost`, loopback and private IP literals; any other
//! host name counts as cloud) unless [`HttpRerankerConfig::allow_cloud`] is set. Requests to
//! anything but this device are recorded in the egress ledger, redirects are never followed and
//! local requests bypass any proxy.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nugget_core::capability::Locality;
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::ItemRef;
use nugget_core::{Error, Result};
use nugget_infer::Secret;
use nugget_infer::locality::classify_url;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Egress-ledger feature tag for rerank requests.
pub const FEATURE_RERANK: &str = "rerank";

/// Largest response body read (a score per passage is a few dozen bytes).
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Scores passages against a query.
#[async_trait]
pub trait Reranker: Send + Sync {
    /// A short name for logs.
    fn name(&self) -> &str;

    /// Where the passages go. Sensitive items (see [`crate::is_sensitive`]) are never sent to a
    /// `Cloud` reranker. Defaults to `Cloud` so an implementation that does not say fails closed.
    fn locality(&self) -> Locality {
        Locality::Cloud
    }

    /// One relevance score per passage, in passage order. Higher is more relevant; the scale is
    /// the model's own (probabilities or logits): the retriever only compares scores of one call.
    async fn rerank(&self, query: &str, passages: &[String]) -> Result<Vec<f32>>;

    /// [`rerank`](Self::rerank) with the items the passages come from, for the egress ledger.
    /// The default ignores `items`.
    async fn rerank_items(
        &self,
        query: &str,
        passages: &[String],
        items: &[ItemRef],
    ) -> Result<Vec<f32>> {
        let _ = items;
        self.rerank(query, passages).await
    }
}

/// Settings for [`HttpReranker`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpRerankerConfig {
    /// Server root, e.g. `http://127.0.0.1:8013`.
    pub base_url: String,
    /// Path of the rerank endpoint under `base_url`. llama-server serves `v1/rerank`, `rerank`
    /// and `v1/reranking`.
    pub path: String,
    /// Model name sent in the request (llama-server ignores it; other servers may not).
    pub model: String,
    /// Whole-request timeout. A slow reranker is skipped, not waited for.
    pub timeout_ms: u64,
    pub connect_timeout_ms: u64,
    /// Characters of each passage sent (cross-encoders have short context windows).
    pub max_passage_chars: usize,
    /// Accept a base URL that is not on this device or the LAN. Off by default: passages are
    /// the user's content.
    pub allow_cloud: bool,
    /// Treat the endpoint as at least this remote (e.g. `local_network` for a server on another
    /// machine reached through a tunnel). Can only widen what the URL says, never narrow it.
    pub locality: Option<Locality>,
    /// Connect to this Unix-domain socket instead of the URL's host (the URL then only names the
    /// path, e.g. `http://localhost`): a local runtime server on a private socket. Unix only;
    /// the endpoint is then on this device.
    pub socket_path: Option<std::path::PathBuf>,
}

impl Default for HttpRerankerConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            path: "v1/rerank".into(),
            model: "reranker".into(),
            timeout_ms: 2_000,
            connect_timeout_ms: 500,
            max_passage_chars: 2_000,
            allow_cloud: false,
            locality: None,
            socket_path: None,
        }
    }
}

/// A [`Reranker`] over HTTP (see the module docs).
pub struct HttpReranker {
    client: reqwest::Client,
    url: reqwest::Url,
    /// Ledger host (`host` or `host:port`).
    host: String,
    locality: Locality,
    model: String,
    max_passage_chars: usize,
    api_key: Option<Secret>,
    egress: Arc<dyn EgressSink>,
}

impl std::fmt::Debug for HttpReranker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpReranker")
            .field("url", &self.url.as_str())
            .field("locality", &self.locality)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

fn remoteness(l: Locality) -> u8 {
    match l {
        Locality::OnDevice => 0,
        Locality::LocalNetwork => 1,
        Locality::Cloud => 2,
    }
}

impl HttpReranker {
    /// Build a client for `config`. `api_key` is sent as a bearer token (the runtime's
    /// per-process key). `Error::Invalid` for a bad URL, `Error::Unsupported` for a cloud URL
    /// without [`HttpRerankerConfig::allow_cloud`] or a plaintext `http` cloud URL.
    pub fn new(
        config: &HttpRerankerConfig,
        api_key: Option<Secret>,
        egress: Arc<dyn EgressSink>,
    ) -> Result<Self> {
        let base = reqwest::Url::parse(config.base_url.trim())
            .map_err(|e| Error::Invalid(format!("reranker base_url {:?}: {e}", config.base_url)))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(Error::Invalid(format!(
                "reranker base_url must be http(s): {base}"
            )));
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(Error::Invalid(
                "reranker base_url must not carry credentials".into(),
            ));
        }
        let mut locality = match &config.socket_path {
            // Traffic stays on this machine whatever the URL's host says.
            Some(_) => Locality::OnDevice,
            None => classify_url(base.as_str())?,
        };
        if let Some(l) = config.locality
            && remoteness(l) > remoteness(locality)
        {
            locality = l;
        }
        if locality == Locality::Cloud {
            if !config.allow_cloud {
                return Err(Error::Unsupported(format!(
                    "reranker {base} is not on this device or the local network (only local \
                     rerankers are used unless allow_cloud is set)"
                )));
            }
            if base.scheme() != "https" {
                return Err(Error::Unsupported(format!(
                    "refusing plaintext http to a cloud reranker: {base}"
                )));
            }
        }
        let mut url = base.clone();
        let joined = format!(
            "{}/{}",
            url.path().trim_end_matches('/'),
            config.path.trim_start_matches('/')
        );
        url.set_path(&joined);

        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("nugget/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_millis(config.timeout_ms.max(1)))
            .connect_timeout(Duration::from_millis(config.connect_timeout_ms.max(1)))
            // A redirect would carry the passages to a host the locality check never saw.
            .redirect(reqwest::redirect::Policy::none());
        if locality != Locality::Cloud {
            builder = builder.no_proxy();
        }
        if let Some(path) = &config.socket_path {
            // Every connection goes to the socket: no DNS, no proxy, no TCP.
            #[cfg(unix)]
            {
                builder = builder.unix_socket(path.clone());
            }
            #[cfg(not(unix))]
            return Err(Error::Unsupported(format!(
                "reranker socket {} (Unix-domain sockets) is not supported on this platform",
                path.display()
            )));
        }
        if let Some(host) = base.host_str()
            && host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        {
            // Never let DNS move `localhost` off this machine.
            let port = base.port_or_known_default().unwrap_or(0);
            builder = builder.resolve_to_addrs(
                host,
                &[
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
                ],
            );
        }
        let client = builder
            .build()
            .map_err(|e| Error::Provider(format!("building reranker HTTP client: {e}")))?;
        let host = match base.port() {
            Some(port) => format!("{}:{port}", base.host_str().unwrap_or_default()),
            None => base.host_str().unwrap_or_default().to_owned(),
        };
        Ok(Self {
            client,
            url,
            host,
            locality,
            model: config.model.clone(),
            max_passage_chars: config.max_passage_chars.max(1),
            api_key,
            egress,
        })
    }

    pub fn locality(&self) -> Locality {
        self.locality
    }

    async fn call(&self, query: &str, passages: &[String], items: &[ItemRef]) -> Result<Vec<f32>> {
        if passages.is_empty() {
            return Ok(Vec::new());
        }
        let documents: Vec<String> = passages
            .iter()
            .map(|p| p.chars().take(self.max_passage_chars).collect())
            .collect();
        let body = serde_json::to_vec(&serde_json::json!({
            "model": self.model,
            "query": query,
            "documents": documents,
            "top_n": documents.len(),
        }))?;
        let mut meter = Meter {
            sink: (self.locality != Locality::OnDevice).then(|| self.egress.clone()),
            host: self.host.clone(),
            items: items.to_vec(),
            sent: body.len() as u64,
            received: 0,
        };
        let mut req = self
            .client
            .post(self.url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        if let Some(key) = &self.api_key {
            let mut value =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {}", key.expose()))
                    .map_err(|_| {
                        Error::Invalid("reranker API key is not a valid header value".into())
                    })?;
            value.set_sensitive(true);
            req = req.header(reqwest::header::AUTHORIZATION, value);
        }
        let mut resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if e.is_connect() {
                    meter.sent = 0;
                }
                return Err(Error::Network(format!("reranker {}: {e}", self.host)));
            }
        };
        let status = resp.status();
        let mut buf: Vec<u8> = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    meter.received = meter.received.saturating_add(chunk.len() as u64);
                    if buf.len() + chunk.len() > MAX_RESPONSE_BYTES {
                        return Err(Error::Provider(format!(
                            "reranker {}: response exceeds {MAX_RESPONSE_BYTES} bytes",
                            self.host
                        )));
                    }
                    buf.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => return Err(Error::Network(format!("reranker {}: {e}", self.host))),
            }
        }
        if !status.is_success() {
            let detail: String = String::from_utf8_lossy(&buf).chars().take(200).collect();
            return Err(Error::Provider(format!(
                "reranker {}: HTTP {status}: {detail}",
                self.host
            )));
        }
        let parsed: RerankResponse = serde_json::from_slice(&buf).map_err(|e| {
            Error::Provider(format!("reranker {}: invalid response: {e}", self.host))
        })?;
        scores_by_index(parsed, passages.len())
    }
}

#[async_trait]
impl Reranker for HttpReranker {
    fn name(&self) -> &str {
        &self.host
    }

    fn locality(&self) -> Locality {
        self.locality
    }

    async fn rerank(&self, query: &str, passages: &[String]) -> Result<Vec<f32>> {
        self.call(query, passages, &[]).await
    }

    async fn rerank_items(
        &self,
        query: &str,
        passages: &[String],
        items: &[ItemRef],
    ) -> Result<Vec<f32>> {
        self.call(query, passages, items).await
    }
}

#[derive(Deserialize)]
struct RerankResponse {
    results: Vec<RerankResult>,
}

#[derive(Deserialize)]
struct RerankResult {
    index: usize,
    /// `relevance_score` (llama-server, Jina, Cohere) or `score` (some other servers).
    #[serde(alias = "score")]
    relevance_score: f64,
}

/// Scores in passage order; every passage must be scored exactly once with a finite score.
fn scores_by_index(resp: RerankResponse, n: usize) -> Result<Vec<f32>> {
    let mut out: Vec<Option<f32>> = vec![None; n];
    for r in resp.results {
        let slot = out.get_mut(r.index).ok_or_else(|| {
            Error::Provider(format!("reranker returned index {} of {n}", r.index))
        })?;
        if slot.is_some() || !r.relevance_score.is_finite() {
            return Err(Error::Provider(
                "reranker returned a duplicate or non-finite score".into(),
            ));
        }
        *slot = Some(r.relevance_score as f32);
    }
    out.into_iter()
        .collect::<Option<Vec<f32>>>()
        .ok_or_else(|| Error::Provider("reranker did not score every passage".into()))
}

/// Records one exchange in the egress ledger when dropped (unless on-device).
struct Meter {
    sink: Option<Arc<dyn EgressSink>>,
    host: String,
    items: Vec<ItemRef>,
    sent: u64,
    received: u64,
}

impl Drop for Meter {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.record(EgressEvent {
                at: OffsetDateTime::now_utc(),
                host: std::mem::take(&mut self.host),
                feature: FEATURE_RERANK.into(),
                bytes_sent: self.sent,
                bytes_received: self.received,
                items: std::mem::take(&mut self.items),
            });
        }
    }
}

/// What the adaptive reranker is doing ([`Retriever::rerank_status`](crate::Retriever::rerank_status)).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RerankStatus {
    /// Candidates the next search will rerank (0 while paused).
    pub pool: usize,
    /// Measured time per passage (moving average), once a rerank has completed.
    pub per_passage_ms: Option<f64>,
    /// Reranking is paused for this much longer (milliseconds), because it could not meet its
    /// budget.
    pub paused_for_ms: Option<u64>,
    /// Why reranking is paused or its pool was cut, for status displays and logs.
    pub reason: Option<String>,
}

/// Keeps the reranker within its budget
/// ([`RetrieveConfig::rerank_adaptive`](crate::RetrieveConfig::rerank_adaptive)). A slow machine
/// (a cross-encoder on a few CPU cores) cannot score the configured pool in time, and a rerank
/// that times out costs the whole timeout and changes nothing. So:
///
/// - after a rerank completes, the pool becomes what fits in 80% of the timeout at the measured
///   time per passage (never above the configured pool);
/// - after a timeout, the pool halves;
/// - when fewer than `rerank_min_pool` passages would fit, reranking pauses for
///   `rerank_cooldown_ms` (doubling on each consecutive pause, up to 8×), then one search probes
///   it again with the minimum pool.
///
/// Shared by clones of a retriever.
#[derive(Debug, Default)]
pub(crate) struct RerankGovernor {
    state: std::sync::Mutex<GovernorState>,
}

#[derive(Debug, Default)]
struct GovernorState {
    pool: Option<usize>,
    per_passage_ms: Option<f64>,
    paused_until: Option<tokio::time::Instant>,
    pauses: u32,
    reason: Option<String>,
}

/// Share of the timeout the adaptive pool is sized to fill.
const BUDGET_SHARE: f64 = 0.8;

impl RerankGovernor {
    fn lock(&self) -> std::sync::MutexGuard<'_, GovernorState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn min_pool(cfg: &crate::RetrieveConfig) -> usize {
        cfg.rerank_min_pool.min(cfg.rerank_pool).max(1)
    }

    /// How many candidates to rerank now; `None` to skip (paused).
    pub(crate) fn plan(&self, cfg: &crate::RetrieveConfig) -> Option<usize> {
        if !cfg.rerank_adaptive {
            return Some(cfg.rerank_pool);
        }
        let mut st = self.lock();
        if let Some(until) = st.paused_until {
            if tokio::time::Instant::now() < until {
                return None;
            }
            st.paused_until = None;
        }
        Some(st.pool.unwrap_or(cfg.rerank_pool).min(cfg.rerank_pool))
    }

    /// A rerank of `n` passages completed in `elapsed`.
    pub(crate) fn completed(&self, cfg: &crate::RetrieveConfig, n: usize, elapsed: Duration) {
        if !cfg.rerank_adaptive || n == 0 {
            return;
        }
        let mut st = self.lock();
        let per = elapsed.as_secs_f64() * 1000.0 / n as f64;
        let per = match st.per_passage_ms {
            Some(prev) => 0.5 * prev + 0.5 * per,
            None => per,
        };
        st.per_passage_ms = Some(per);
        let budget = cfg.rerank_timeout_ms.max(1) as f64 * BUDGET_SHARE;
        let fit = ((budget / per.max(1e-3)).floor() as usize).min(cfg.rerank_pool);
        if fit < Self::min_pool(cfg) {
            let reason = format!(
                "reranking takes {per:.0} ms per passage; fewer than {} passages fit the {} ms \
                 budget",
                Self::min_pool(cfg),
                cfg.rerank_timeout_ms
            );
            Self::pause(&mut st, cfg, reason);
        } else {
            st.pauses = 0;
            st.reason = (fit < cfg.rerank_pool).then(|| {
                format!(
                    "reranking {fit} of {} candidates to fit the {} ms budget ({per:.0} ms per \
                     passage)",
                    cfg.rerank_pool, cfg.rerank_timeout_ms
                )
            });
            st.pool = Some(fit);
        }
    }

    /// A rerank of `n` passages timed out.
    pub(crate) fn timed_out(&self, cfg: &crate::RetrieveConfig, n: usize) {
        if !cfg.rerank_adaptive {
            return;
        }
        let mut st = self.lock();
        let half = n / 2;
        if half < Self::min_pool(cfg) {
            let reason = format!(
                "reranking {n} passages timed out after {} ms",
                cfg.rerank_timeout_ms
            );
            Self::pause(&mut st, cfg, reason);
        } else {
            st.reason = Some(format!(
                "reranking {n} passages timed out after {} ms; trying {half}",
                cfg.rerank_timeout_ms
            ));
            st.pool = Some(half);
        }
    }

    fn pause(st: &mut GovernorState, cfg: &crate::RetrieveConfig, reason: String) {
        let factor = 1u64 << st.pauses.min(3);
        st.pauses = st.pauses.saturating_add(1);
        let cooldown = Duration::from_millis(cfg.rerank_cooldown_ms.saturating_mul(factor));
        st.paused_until = Some(tokio::time::Instant::now() + cooldown);
        // The next probe asks for as little as is worth reranking.
        st.pool = Some(Self::min_pool(cfg));
        tracing::info!(%reason, cooldown_ms = cooldown.as_millis() as u64, "search reranking paused");
        st.reason = Some(format!("{reason}; paused for {} s", cooldown.as_secs()));
    }

    pub(crate) fn status(&self, cfg: &crate::RetrieveConfig) -> RerankStatus {
        let st = self.lock();
        let now = tokio::time::Instant::now();
        let paused_for_ms = st
            .paused_until
            .filter(|u| *u > now)
            .map(|u| (u - now).as_millis() as u64);
        RerankStatus {
            pool: if paused_for_ms.is_some() {
                0
            } else {
                st.pool.unwrap_or(cfg.rerank_pool).min(cfg.rerank_pool)
            },
            per_passage_ms: st.per_passage_ms,
            paused_for_ms,
            reason: st.reason.clone(),
        }
    }
}

/// Map raw reranker scores into `[0, 1]`, best = 1. Scores already in `[0, 1]` (probabilities)
/// are kept as they are; anything else (logits) goes through a logistic first. Then divide by
/// the maximum, as first-stage relevance is.
pub(crate) fn normalise_scores(raw: &[f32]) -> Vec<f32> {
    let probs = raw.iter().all(|s| (0.0..=1.0).contains(s));
    let mapped: Vec<f32> = raw
        .iter()
        .map(|&s| if probs { s } else { 1.0 / (1.0 + (-s).exp()) })
        .collect();
    let max = mapped.iter().copied().fold(0.0f32, f32::max);
    if max > 0.0 {
        mapped.iter().map(|s| s / max).collect()
    } else {
        vec![0.0; raw.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::egress::NullEgress;

    fn cfg(url: &str) -> HttpRerankerConfig {
        HttpRerankerConfig {
            base_url: url.into(),
            ..Default::default()
        }
    }

    #[test]
    fn only_local_urls_are_accepted_by_default() {
        let egress: Arc<dyn EgressSink> = Arc::new(NullEgress);
        for ok in [
            "http://127.0.0.1:8013",
            "http://localhost:8013/",
            "http://[::1]:9",
            "http://192.168.1.20:8080",
        ] {
            assert!(
                HttpReranker::new(&cfg(ok), None, egress.clone()).is_ok(),
                "{ok}"
            );
        }
        for bad in [
            "https://api.example.com",
            "http://gpu-box:8080",
            "http://8.8.8.8",
            "http://foo.localhost:1",
        ] {
            let e = HttpReranker::new(&cfg(bad), None, egress.clone()).unwrap_err();
            assert!(matches!(e, Error::Unsupported(_)), "{bad}: {e:?}");
        }
        for invalid in ["not a url", "ftp://127.0.0.1", "http://u:p@127.0.0.1"] {
            assert!(HttpReranker::new(&cfg(invalid), None, egress.clone()).is_err());
        }
        // Explicitly allowed: https only.
        let mut c = cfg("https://api.example.com");
        c.allow_cloud = true;
        let r = HttpReranker::new(&c, None, egress.clone()).unwrap();
        assert_eq!(r.locality(), Locality::Cloud);
        c.base_url = "http://api.example.com".into();
        assert!(HttpReranker::new(&c, None, egress.clone()).is_err());
        // The locality override only widens.
        let mut c = cfg("http://192.168.1.20:8080");
        c.locality = Some(Locality::OnDevice);
        let r = HttpReranker::new(&c, None, egress).unwrap();
        assert_eq!(r.locality(), Locality::LocalNetwork);
    }

    /// A runtime server on a private Unix socket: on-device whatever the URL says.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_socket_endpoint_is_on_device_and_reached_through_the_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sock =
            std::env::temp_dir().join(format!("nugget-rerank-{}-{nanos}.sock", std::process::id()));
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 64 * 1024];
            let mut got = Vec::new();
            // Read the head and the body (content-length), then answer.
            loop {
                let n = conn.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len: usize = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if got.len() >= end + 4 + len {
                        break;
                    }
                }
                assert!(n > 0, "connection closed early");
            }
            let body = r#"{"results":[{"index":0,"relevance_score":0.1},{"index":1,"relevance_score":0.9}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            conn.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&got).to_string()
        });
        let c = HttpRerankerConfig {
            base_url: "http://localhost".into(),
            socket_path: Some(sock.clone()),
            ..Default::default()
        };
        let r = HttpReranker::new(&c, Some(Secret::new("k".to_string())), Arc::new(NullEgress))
            .unwrap();
        assert_eq!(r.locality(), Locality::OnDevice);
        let scores = r.rerank("q", &["a".into(), "b".into()]).await.unwrap();
        assert_eq!(scores, vec![0.1, 0.9]);
        let request = server.await.unwrap();
        let _ = std::fs::remove_file(&sock);
        assert!(request.starts_with("POST /v1/rerank "), "{request}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer k")
        );
    }

    #[test]
    fn scores_map_back_to_passage_order() {
        let resp = RerankResponse {
            results: vec![
                RerankResult {
                    index: 2,
                    relevance_score: 5.0,
                },
                RerankResult {
                    index: 0,
                    relevance_score: -1.0,
                },
                RerankResult {
                    index: 1,
                    relevance_score: 0.5,
                },
            ],
        };
        assert_eq!(scores_by_index(resp, 3).unwrap(), vec![-1.0, 0.5, 5.0]);
        let missing = RerankResponse {
            results: vec![RerankResult {
                index: 0,
                relevance_score: 1.0,
            }],
        };
        assert!(scores_by_index(missing, 2).is_err());
        let out_of_range = RerankResponse {
            results: vec![RerankResult {
                index: 5,
                relevance_score: 1.0,
            }],
        };
        assert!(scores_by_index(out_of_range, 1).is_err());
    }

    #[test]
    fn normalisation() {
        assert_eq!(normalise_scores(&[0.2, 0.4, 0.1]), vec![0.5, 1.0, 0.25]);
        let logits = normalise_scores(&[-2.0, 0.0, 3.0]);
        assert_eq!(logits[2], 1.0);
        assert!(logits[0] < logits[1] && logits[1] < logits[2]);
        assert_eq!(normalise_scores(&[0.0, 0.0]), vec![0.0, 0.0]);
        assert!(normalise_scores(&[]).is_empty());
    }

    fn adaptive() -> crate::RetrieveConfig {
        crate::RetrieveConfig {
            rerank_pool: 30,
            rerank_min_pool: 8,
            rerank_timeout_ms: 2_500,
            rerank_cooldown_ms: 60_000,
            ..Default::default()
        }
    }

    #[test]
    fn governor_halves_on_timeouts_then_pauses() {
        let cfg = adaptive();
        let g = RerankGovernor::default();
        assert_eq!(g.plan(&cfg), Some(30));
        g.timed_out(&cfg, 30);
        assert_eq!(g.plan(&cfg), Some(15));
        g.timed_out(&cfg, 15);
        // 7 would be below the minimum pool: pause.
        assert_eq!(g.plan(&cfg), None);
        let st = g.status(&cfg);
        assert_eq!(st.pool, 0);
        assert!(
            st.paused_for_ms
                .is_some_and(|ms| ms > 50_000 && ms <= 60_000)
        );
        assert!(st.reason.unwrap().contains("timed out"));
    }

    #[test]
    fn governor_sizes_the_pool_from_measured_time() {
        let cfg = adaptive();
        let g = RerankGovernor::default();
        // 30 passages in 3 s: 100 ms each; 80% of 2.5 s fits 20.
        g.completed(&cfg, 30, Duration::from_secs(3));
        assert_eq!(g.plan(&cfg), Some(20));
        assert!(g.status(&cfg).reason.unwrap().contains("20 of 30"));
        // Fast again: back to the configured pool (moving average 55 ms → fits 36, capped).
        g.completed(&cfg, 20, Duration::from_millis(200));
        assert_eq!(g.plan(&cfg), Some(30));
        assert_eq!(g.status(&cfg).reason, None);
        // Far too slow (500 ms per passage on average): pause, then probe with the minimum.
        g.completed(&cfg, 10, Duration::from_secs(10));
        assert_eq!(g.plan(&cfg), None);
    }

    #[test]
    fn a_paused_governor_probes_with_the_minimum_pool_after_its_cooldown() {
        let cfg = crate::RetrieveConfig {
            rerank_cooldown_ms: 0,
            ..adaptive()
        };
        let g = RerankGovernor::default();
        g.timed_out(&cfg, 10);
        assert_eq!(g.plan(&cfg), Some(8));
        // Not adaptive: always the configured pool.
        let fixed = crate::RetrieveConfig {
            rerank_adaptive: false,
            ..adaptive()
        };
        g.timed_out(&fixed, 30);
        assert_eq!(g.plan(&fixed), Some(30));
    }
}
