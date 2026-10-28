//! Shared HTTP plumbing: client construction, egress metering, retries and error mapping.

use std::collections::hash_map::RandomState;
use std::future::Future;
use std::hash::{BuildHasher, Hasher};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use nugget_core::capability::Locality;
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::{Error, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use time::OffsetDateTime;
use url::Url;

use crate::sse::SseParser;
use crate::types::{ChatEvent, ChatStats, ChatStream, Timings};

/// Largest error body we read into memory.
const MAX_ERROR_BODY: usize = 64 * 1024;
/// Largest non-streaming JSON body we accept (embeddings can be big).
const MAX_JSON_BODY: usize = 256 * 1024 * 1024;
/// How long an idle pooled connection is kept for reuse. Long enough that a prewarm on trigger
/// press (or a previous draft) still has a live TLS session when the request is sent.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// HTTP/2 PING interval on pooled connections, so middleboxes and servers do not silently drop
/// them while idle (a dead pooled connection costs a full reconnect on the next request).
const H2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const H2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);
const TCP_KEEPALIVE: Duration = Duration::from_secs(60);

tokio::task_local! {
    /// Where the connector records how long opening a connection took, for the request being
    /// sent on this task (see [`ConnectTiming`]).
    static CONNECT_SLOT: Arc<Mutex<Option<Duration>>>;
}

/// Tower layer around reqwest's connector (DNS + TCP + TLS) that times each new connection and
/// reports it to the request whose task started it. A connection opened in the background (the
/// pool handed the request another one first) reports nothing, which is right: that request did
/// not wait for it.
#[derive(Clone, Copy)]
struct ConnectTiming;

impl<S> tower_layer::Layer<S> for ConnectTiming {
    type Service = ConnectTimed<S>;
    fn layer(&self, inner: S) -> Self::Service {
        ConnectTimed(inner)
    }
}

#[derive(Clone)]
struct ConnectTimed<S>(S);

impl<S, R> tower_service::Service<R> for ConnectTimed<S>
where
    S: tower_service::Service<R>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = std::result::Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<std::result::Result<(), S::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, req: R) -> Self::Future {
        let slot = CONNECT_SLOT.try_with(Arc::clone).ok();
        let started = Instant::now();
        let fut = self.0.call(req);
        Box::pin(async move {
            let out = fut.await;
            if out.is_ok()
                && let Some(slot) = slot
            {
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(started.elapsed());
            }
            out
        })
    }
}

/// When a request started and what its connection cost; turned into [`Timings`] by the stream.
#[derive(Debug, Clone)]
pub(crate) struct RequestClock {
    pub started: Instant,
    pub connect: Option<Duration>,
    pub headers: Duration,
    pub attempts: u32,
}

impl RequestClock {
    pub(crate) fn timings(
        &self,
        first_byte: Option<Instant>,
        first_token: Option<Instant>,
        end: Instant,
    ) -> Timings {
        let since = |t: Instant| t.saturating_duration_since(self.started);
        Timings {
            connect: self.connect,
            headers: self.headers,
            first_byte: first_byte.map(since),
            first_token: first_token.map(since),
            total: since(end),
            attempts: self.attempts,
        }
    }
}

/// Records one HTTP exchange in the egress ledger when dropped (exactly once), unless the
/// provider is on-device.
pub(crate) struct EgressMeter {
    sink: Option<Arc<dyn EgressSink>>,
    host: String,
    feature: String,
    items: Vec<ItemRef>,
    pub(crate) bytes_sent: u64,
    pub(crate) bytes_received: u64,
}

impl EgressMeter {
    pub(crate) fn add_received(&mut self, n: usize) {
        self.bytes_received = self.bytes_received.saturating_add(n as u64);
    }
}

impl Drop for EgressMeter {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.record(EgressEvent {
                at: OffsetDateTime::now_utc(),
                host: std::mem::take(&mut self.host),
                feature: std::mem::take(&mut self.feature),
                bytes_sent: self.bytes_sent,
                bytes_received: self.bytes_received,
                items: std::mem::take(&mut self.items),
            });
        }
    }
}

/// Connection settings shared by all adapters.
#[derive(Clone)]
pub(crate) struct HttpSettings {
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_retries: u32,
    pub retry_base: Duration,
    /// Never wait longer than this for a single retry; a longer `retry-after` fails fast.
    pub max_retry_delay: Duration,
    /// Extra headers (already resolved and marked sensitive).
    pub headers: Vec<(HeaderName, HeaderValue)>,
    /// Connect to exactly these addresses for this host (see [`crate::locality`]).
    pub pin: Option<(String, Vec<std::net::SocketAddr>)>,
    /// Bypass any proxy: the endpoint is on this machine or the LAN.
    pub direct: bool,
}

/// One provider's HTTP endpoint.
pub(crate) struct Http {
    client: reqwest::Client,
    pub(crate) provider: ProviderId,
    pub(crate) base: Url,
    /// Ledger host (`host` or `host:port`).
    pub(crate) host: String,
    pub(crate) locality: Locality,
    egress: Arc<dyn EgressSink>,
    settings: HttpSettings,
    auth: Vec<(HeaderName, HeaderValue)>,
}

impl Http {
    pub(crate) fn new(
        provider: ProviderId,
        base: Url,
        locality: Locality,
        egress: Arc<dyn EgressSink>,
        settings: HttpSettings,
        auth: Vec<(HeaderName, HeaderValue)>,
    ) -> Result<Self> {
        let mut default_headers = HeaderMap::new();
        for (name, value) in &settings.headers {
            let mut value = value.clone();
            value.set_sensitive(true);
            default_headers.insert(name.clone(), value);
        }
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("nugget/", env!("CARGO_PKG_VERSION")))
            .default_headers(default_headers)
            .connect_timeout(settings.connect_timeout)
            .read_timeout(settings.idle_timeout)
            // Connection reuse (M3 latency): keep TLS sessions warm between requests and after a
            // prewarm. HTTP/2 is negotiated by ALPN on https; plain-http local servers use
            // HTTP/1.1 keep-alive from the same pool.
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .tcp_nodelay(true)
            .tcp_keepalive(TCP_KEEPALIVE)
            .http2_keep_alive_interval(H2_KEEPALIVE_INTERVAL)
            .http2_keep_alive_timeout(H2_KEEPALIVE_TIMEOUT)
            .http2_keep_alive_while_idle(true)
            .connector_layer(ConnectTiming)
            // Never follow redirects: a redirect would carry the key, headers and body to a host
            // the egress ledger and locality checks never saw. A 3xx is an error instead.
            .redirect(reqwest::redirect::Policy::none());
        if settings.direct {
            // Requests to this machine or the LAN must never be routed through a proxy.
            builder = builder.no_proxy();
        }
        if let Some((host, addrs)) = &settings.pin {
            // Connect only to the addresses classified at build time (DNS-rebinding guard).
            builder = builder.resolve_to_addrs(host, addrs);
        }
        let client = builder
            .build()
            .map_err(|e| Error::Provider(format!("{provider}: building HTTP client: {e}")))?;
        let host = match base.port() {
            Some(port) => format!("{}:{port}", base.host_str().unwrap_or_default()),
            None => base.host_str().unwrap_or_default().to_string(),
        };
        Ok(Self {
            client,
            provider,
            base,
            host,
            locality,
            egress,
            settings,
            auth,
        })
    }

    /// `base` joined with a relative path (`"v1/messages"`), keeping any base path prefix.
    pub(crate) fn endpoint(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        let joined = format!(
            "{}/{}",
            url.path().trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        url.set_path(&joined);
        url
    }

    fn meter(&self, feature: &str, items: &[ItemRef], bytes_sent: u64) -> EgressMeter {
        EgressMeter {
            sink: (self.locality != Locality::OnDevice).then(|| self.egress.clone()),
            host: self.host.clone(),
            feature: feature.to_string(),
            items: items.to_vec(),
            bytes_sent,
            bytes_received: 0,
        }
    }

    /// POST a JSON body, retrying transient failures (connect errors, timeouts before the
    /// response, 408/409/429/5xx/529) with backoff. Returns the successful response and the meter
    /// that will record its egress once dropped. Once retries run out, 429/529 surface as
    /// [`Error::RateLimited`] and transport failures as [`Error::Network`].
    pub(crate) async fn post_json(
        &self,
        path: &str,
        body: &Value,
        feature: &str,
        items: &[ItemRef],
        accept_sse: bool,
    ) -> Result<(reqwest::Response, EgressMeter, RequestClock)> {
        let bytes = Bytes::from(serde_json::to_vec(body)?);
        self.send(Method::POST, path, Some(bytes), feature, items, accept_sse)
            .await
    }

    /// GET with no body (prewarm). Same retry, metering and error rules as [`Self::post_json`].
    pub(crate) async fn get(
        &self,
        path: &str,
        feature: &str,
    ) -> Result<(reqwest::Response, EgressMeter, RequestClock)> {
        self.send(Method::GET, path, None, feature, &[], false)
            .await
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        bytes: Option<Bytes>,
        feature: &str,
        items: &[ItemRef],
        accept_sse: bool,
    ) -> Result<(reqwest::Response, EgressMeter, RequestClock)> {
        let url = self.endpoint(path);
        let started = Instant::now();
        let mut connect: Option<Duration> = None;
        let mut attempt: u32 = 0;
        loop {
            let mut req = self.client.request(method.clone(), url.clone());
            if let Some(bytes) = &bytes {
                req = req
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(bytes.clone());
            }
            if accept_sse {
                req = req.header(reqwest::header::ACCEPT, "text/event-stream");
            }
            for (k, v) in &self.auth {
                req = req.header(k.clone(), v.clone());
            }
            let sent = bytes.as_ref().map_or(0, |b| b.len() as u64);
            let mut meter = self.meter(feature, items, sent);
            let can_retry = attempt < self.settings.max_retries;
            let slot = Arc::new(Mutex::new(None));
            let result = CONNECT_SLOT.scope(slot.clone(), req.send()).await;
            if let Some(c) = *slot.lock().unwrap_or_else(|e| e.into_inner()) {
                connect = Some(connect.unwrap_or_default() + c);
            }
            match result {
                Ok(resp) if resp.status().is_success() => {
                    let clock = RequestClock {
                        started,
                        connect,
                        headers: started.elapsed(),
                        attempts: attempt + 1,
                    };
                    return Ok((resp, meter, clock));
                }
                Ok(resp) if resp.status().is_redirection() => {
                    let target = redirect_target(&url, resp.headers());
                    let body = read_limited(resp, MAX_ERROR_BODY).await;
                    meter.add_received(body.len());
                    drop(meter);
                    return Err(Error::Provider(format!(
                        "{}: unexpected redirect to {target} (redirects are not followed; \
                         check base_url)",
                        self.provider
                    )));
                }
                Ok(resp) => {
                    let status = resp.status();
                    let delay = retry_after(resp.headers());
                    let body = read_limited(resp, MAX_ERROR_BODY).await;
                    meter.add_received(body.len());
                    drop(meter);
                    let err = http_error(&self.provider, status, &body, delay);
                    if can_retry && is_retryable_status(status) {
                        let wait = delay.unwrap_or_else(|| self.backoff(attempt));
                        if wait <= self.settings.max_retry_delay {
                            tracing::warn!(provider = %self.provider, %status, ?wait, attempt, "retrying");
                            tokio::time::sleep(wait).await;
                            attempt += 1;
                            continue;
                        }
                    }
                    return Err(err);
                }
                Err(e) => {
                    if e.is_connect() {
                        meter.bytes_sent = 0;
                    }
                    drop(meter);
                    let transient = e.is_connect() || e.is_timeout();
                    if can_retry && transient {
                        let wait = self.backoff(attempt);
                        tracing::warn!(provider = %self.provider, error = %e, ?wait, attempt, "retrying");
                        tokio::time::sleep(wait).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(transport_error(&self.provider, &e));
                }
            }
        }
    }

    /// POST and parse a non-streaming JSON response.
    pub(crate) async fn post_for_json(
        &self,
        path: &str,
        body: &Value,
        feature: &str,
        items: &[ItemRef],
    ) -> Result<Value> {
        let (resp, mut meter, _) = self.post_json(path, body, feature, items, false).await?;
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| transport_error(&self.provider, &e))?;
            meter.add_received(chunk.len());
            if buf.len() + chunk.len() > MAX_JSON_BODY {
                return Err(Error::Provider(format!(
                    "{}: response body exceeds {MAX_JSON_BODY} bytes",
                    self.provider
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&buf)
            .map_err(|e| Error::Provider(format!("{}: invalid JSON response: {e}", self.provider)))
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let base = self.settings.retry_base.as_millis() as u64;
        let exp = base.saturating_mul(1u64 << attempt.min(16));
        let jitter = if base == 0 {
            0
        } else {
            let mut h = RandomState::new().build_hasher();
            h.write_u32(attempt);
            h.finish() % base
        };
        Duration::from_millis(exp.saturating_add(jitter)).min(self.settings.max_retry_delay)
    }
}

/// Converts SSE events into chat events. One decoder per response.
pub(crate) trait SseDecoder: Send + 'static {
    fn on_event(&mut self, event: crate::sse::SseEvent) -> Result<Vec<ChatEvent>>;
    /// The body ended. Return trailing events (usually `Done`) or an error if incomplete.
    fn on_eof(&mut self) -> Result<Vec<ChatEvent>>;
    /// Prompt-cache usage seen so far: (tokens read from cache, tokens written to cache).
    fn cache_usage(&self) -> (Option<u32>, Option<u32>) {
        (None, None)
    }
}

/// The [`ChatEvent::Stats`] sent just before `done`.
fn stats_for(
    done: &ChatEvent,
    cache: (Option<u32>, Option<u32>),
    clock: &RequestClock,
    first_byte: Option<Instant>,
    first_token: Option<Instant>,
) -> ChatEvent {
    let (input_tokens, output_tokens) = match done {
        ChatEvent::Done {
            input_tokens,
            output_tokens,
            ..
        } => (*input_tokens, *output_tokens),
        _ => (None, None),
    };
    ChatEvent::Stats(ChatStats {
        input_tokens,
        output_tokens,
        cached_input_tokens: cache.0,
        cache_write_tokens: cache.1,
        timings: Some(clock.timings(first_byte, first_token, Instant::now())),
    })
}

fn is_token(e: &ChatEvent) -> bool {
    match e {
        ChatEvent::TextDelta(t) => !t.is_empty(),
        ChatEvent::ToolCall { .. } => true,
        _ => false,
    }
}

/// Turn a successful streaming response into a [`ChatStream`]. The stream ends after the first
/// `Done` or error; `Done` is preceded by a [`ChatEvent::Stats`] with cache usage and timings.
/// The egress meter records when the stream finishes or is dropped.
pub(crate) fn sse_chat_stream<D: SseDecoder>(
    provider: ProviderId,
    resp: reqwest::Response,
    meter: EgressMeter,
    clock: RequestClock,
    mut decoder: D,
) -> ChatStream {
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    Box::pin(async_stream::stream! {
        let mut meter = meter;
        let mut first_byte: Option<Instant> = None;
        let mut first_token: Option<Instant> = None;
        if content_type.contains("application/json") {
            // Server ignored `stream: true` or answered 200 with an error envelope.
            let body = read_limited(resp, MAX_ERROR_BODY).await;
            meter.add_received(body.len());
            let detail = error_detail(&body).unwrap_or_else(|| truncate(&body, 300));
            yield Err(Error::Provider(format!(
                "{provider}: expected an event stream, got JSON: {detail}"
            )));
            return;
        }
        let mut body = resp.bytes_stream();
        let mut parser = SseParser::new();
        loop {
            let (events, eof) = match body.next().await {
                Some(Ok(chunk)) => {
                    meter.add_received(chunk.len());
                    if !chunk.is_empty() {
                        first_byte.get_or_insert_with(Instant::now);
                    }
                    match parser.push(&chunk) {
                        Ok(evs) => (evs, false),
                        Err(_) => {
                            yield Err(Error::Provider(format!("{provider}: SSE line too long")));
                            return;
                        }
                    }
                }
                Some(Err(e)) => {
                    yield Err(transport_error(&provider, &e));
                    return;
                }
                None => (parser.finish(), true),
            };
            for ev in events {
                match decoder.on_event(ev) {
                    Ok(out) => {
                        for e in out {
                            if is_token(&e) {
                                first_token.get_or_insert_with(Instant::now);
                            }
                            let done = matches!(e, ChatEvent::Done { .. });
                            if done {
                                yield Ok(stats_for(&e, decoder.cache_usage(), &clock, first_byte, first_token));
                            }
                            yield Ok(e);
                            if done {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
            if eof {
                match decoder.on_eof() {
                    Ok(out) => {
                        let mut done = false;
                        for e in out {
                            if is_token(&e) {
                                first_token.get_or_insert_with(Instant::now);
                            }
                            if matches!(e, ChatEvent::Done { .. }) {
                                done = true;
                                yield Ok(stats_for(&e, decoder.cache_usage(), &clock, first_byte, first_token));
                            }
                            yield Ok(e);
                        }
                        if !done {
                            yield Err(Error::Provider(format!(
                                "{provider}: stream ended before completion"
                            )));
                        }
                    }
                    Err(e) => yield Err(e),
                }
                return;
            }
        }
    })
}

pub(crate) fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        408 | 409 | 429 | 500 | 502 | 503 | 504 | 529
    )
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    if let Some(ms) = headers
        .get("retry-after-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|ms| ms.is_finite() && *ms >= 0.0)
    {
        return Some(Duration::from_millis(ms as u64));
    }
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(Duration::from_secs_f64)
}

/// The host (`host` or `host:port`) a 3xx `Location` points at, for the error message.
fn redirect_target(from: &Url, headers: &HeaderMap) -> String {
    let Some(loc) = headers
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
    else {
        return "<no location>".into();
    };
    match from.join(loc) {
        Ok(u) => match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            (None, _) => "<no host>".into(),
        },
        Err(_) => "<invalid location>".into(),
    }
}

async fn read_limited(resp: reqwest::Response, limit: usize) -> String {
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(Ok(chunk)) = stream.next().await {
        let room = limit.saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if buf.len() >= limit {
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Extract `type: message` from the common error envelopes:
/// Anthropic `{"type":"error","error":{"type","message"}}`, OpenAI `{"error":{"message","type","code"}}`,
/// Ollama/llama.cpp `{"error":"..."}` or `{"error":{"message","code"}}`.
pub(crate) fn error_detail(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    error_detail_value(&v)
}

pub(crate) fn error_detail_value(v: &Value) -> Option<String> {
    let err = v.get("error").unwrap_or(v);
    if let Some(s) = err.as_str() {
        return Some(s.to_string());
    }
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| v.get("message").and_then(Value::as_str))?;
    let kind = err
        .get("type")
        .and_then(Value::as_str)
        .filter(|t| *t != "error")
        .map(str::to_string)
        .or_else(|| {
            err.get("code").and_then(|c| match c {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
        });
    Some(match kind {
        Some(k) => format!("{k}: {message}"),
        None => message.to_string(),
    })
}

/// Map a non-success response. 429 (too many requests) and 529 (Anthropic "overloaded") become
/// [`Error::RateLimited`] with the server's retry hint; the body detail is logged, since that
/// variant carries none. Everything else is [`Error::Provider`].
fn http_error(
    provider: &ProviderId,
    status: StatusCode,
    body: &str,
    retry_after: Option<Duration>,
) -> Error {
    let detail = error_detail(body).unwrap_or_else(|| truncate(body, 300));
    if is_rate_limit_status(status) {
        tracing::warn!(%provider, status = status.as_u16(), %detail, ?retry_after, "rate limited");
        return Error::RateLimited {
            retry_after_ms: retry_after.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        };
    }
    let hint = match status.as_u16() {
        401 | 403 => " (check the provider's api_key)",
        404 => " (check base_url and model)",
        _ => "",
    };
    let retry = retry_after
        .map(|d| format!(" (retry after {}s)", d.as_secs_f64().ceil()))
        .unwrap_or_default();
    Error::Provider(format!(
        "{provider}: HTTP {}: {detail}{hint}{retry}",
        status.as_u16()
    ))
}

/// Statuses that mean "back off": 429 and Anthropic's 529 (overloaded).
pub(crate) fn is_rate_limit_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 529)
}

/// Map a reqwest failure (sending, or reading the body). Connect, timeout and other transport
/// failures are [`Error::Network`]. A request reqwest could not even build (a bad URL or header
/// from this provider's config) stays [`Error::Provider`], so the Router still falls back.
pub(crate) fn transport_error(provider: &ProviderId, e: &reqwest::Error) -> Error {
    let kind = if e.is_connect() {
        "connection failed"
    } else if e.is_timeout() {
        "timed out"
    } else {
        "request failed"
    };
    // The URL in reqwest's message never carries secrets: keys travel only in headers.
    let msg = match std::error::Error::source(e) {
        Some(src) => format!("{e}: {src}"),
        None => e.to_string(),
    };
    if e.is_builder() {
        return Error::Provider(format!("{provider}: {kind}: {msg}"));
    }
    Error::Network(format!("{provider}: {kind}: {msg}"))
}

/// Parse a tool-call arguments string. Empty means `{}`; invalid JSON is returned as a JSON
/// string holding the raw text (see [`ChatEvent::ToolCall`]).
pub(crate) fn parse_tool_arguments(provider: &ProviderId, name: &str, raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(Default::default());
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%provider, tool = name, error = %e, "tool call arguments are not valid JSON");
            Value::String(raw.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_envelopes() {
        assert_eq!(
            error_detail(
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            )
            .unwrap(),
            "overloaded_error: Overloaded"
        );
        assert_eq!(
            error_detail(r#"{"error":{"message":"bad key","type":"invalid_request_error","code":"invalid_api_key"}}"#)
                .unwrap(),
            "invalid_request_error: bad key"
        );
        assert_eq!(
            error_detail(r#"{"error":"model not found"}"#).unwrap(),
            "model not found"
        );
        assert_eq!(
            error_detail(r#"{"error":{"code":503,"message":"Loading model"}}"#).unwrap(),
            "503: Loading model"
        );
        assert!(error_detail("<html>").is_none());
    }

    #[test]
    fn retry_after_parsing() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("2"));
        assert_eq!(retry_after(&h), Some(Duration::from_secs(2)));
        h.insert("retry-after-ms", HeaderValue::from_static("150"));
        assert_eq!(retry_after(&h), Some(Duration::from_millis(150)));
        let mut h = HeaderMap::new();
        h.insert(
            "retry-after",
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&h), None);
    }

    #[test]
    fn retryable_statuses() {
        for s in [408u16, 409, 429, 500, 502, 503, 504, 529] {
            assert!(is_retryable_status(StatusCode::from_u16(s).unwrap()), "{s}");
        }
        for s in [400u16, 401, 403, 404, 413, 422] {
            assert!(
                !is_retryable_status(StatusCode::from_u16(s).unwrap()),
                "{s}"
            );
        }
    }

    #[test]
    fn rate_limit_statuses_map_to_rate_limited() {
        let p = ProviderId::new("p");
        for s in [429u16, 529] {
            let status = StatusCode::from_u16(s).unwrap();
            assert!(is_rate_limit_status(status), "{s}");
            assert!(matches!(
                http_error(&p, status, "{}", Some(Duration::from_millis(1500))),
                Error::RateLimited {
                    retry_after_ms: Some(1500)
                }
            ));
            assert!(matches!(
                http_error(&p, status, "", None),
                Error::RateLimited {
                    retry_after_ms: None
                }
            ));
        }
        for s in [500u16, 503, 401, 404] {
            let status = StatusCode::from_u16(s).unwrap();
            assert!(!is_rate_limit_status(status), "{s}");
            assert!(matches!(
                http_error(&p, status, "", None),
                Error::Provider(_)
            ));
        }
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let s = "é".repeat(400);
        let t = truncate(&s, 301);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn tool_argument_parsing() {
        let p = ProviderId::new("p");
        assert_eq!(parse_tool_arguments(&p, "f", ""), serde_json::json!({}));
        assert_eq!(
            parse_tool_arguments(&p, "f", r#"{"a":1}"#),
            serde_json::json!({"a":1})
        );
        assert_eq!(
            parse_tool_arguments(&p, "f", r#"{"a":"#),
            Value::String(r#"{"a":"#.into())
        );
    }
}
