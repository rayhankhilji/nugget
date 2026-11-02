//! Operations: request ids, one structured log line per request, Prometheus metrics, optional
//! Sentry-compatible error reports, and the shutdown signal.
//!
//! Nothing here logs or reports personal data: no emails, tokens, prompts, message bodies, user
//! ids or client IPs. A request is identified by its request id only.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::Instrument;

// ---- request ids --------------------------------------------------------------------------------

/// A request id from the caller (`x-request-id`, or Fly's `fly-request-id`) if it is short and
/// plain, else a new one. Never trusted for anything but correlating logs.
pub fn request_id(headers: &axum::http::HeaderMap) -> String {
    ["x-request-id", "fly-request-id"]
        .iter()
        .filter_map(|k| headers.get(*k)?.to_str().ok())
        .map(str::trim)
        .find(|v| {
            (8..=128).contains(&v.len())
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        })
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string())
}

/// The request id of the request being handled (set by [`observe`]).
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

// ---- metrics ------------------------------------------------------------------------------------

const BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];

/// Process-wide counters, rendered as Prometheus text by [`Metrics::render`].
pub struct Metrics {
    requests: Mutex<BTreeMap<(String, u16), u64>>,
    latency_buckets: Vec<AtomicU64>,
    latency_sum_micros: AtomicU64,
    latency_count: AtomicU64,
    rate_limited: Mutex<BTreeMap<&'static str, u64>>,
    sync_runs: Mutex<BTreeMap<&'static str, u64>>,
    gateway_cost_micros: AtomicU64,
}

pub static METRICS: LazyLock<Metrics> = LazyLock::new(|| Metrics {
    requests: Mutex::new(BTreeMap::new()),
    latency_buckets: BUCKETS.iter().map(|_| AtomicU64::new(0)).collect(),
    latency_sum_micros: AtomicU64::new(0),
    latency_count: AtomicU64::new(0),
    rate_limited: Mutex::new(BTreeMap::new()),
    sync_runs: Mutex::new(BTreeMap::new()),
    gateway_cost_micros: AtomicU64::new(0),
});

fn bump<K: Ord>(m: &Mutex<BTreeMap<K, u64>>, k: K) {
    *m.lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(k)
        .or_default() += 1;
}

impl Metrics {
    pub fn record_request(&self, route: &str, status: u16, elapsed: Duration) {
        bump(&self.requests, (route.to_owned(), status));
        let secs = elapsed.as_secs_f64();
        for (b, n) in BUCKETS.iter().zip(&self.latency_buckets) {
            if secs <= *b {
                n.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.latency_sum_micros
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn rate_limited(&self, scope: &'static str) {
        bump(&self.rate_limited, scope);
    }

    pub fn sync_run(&self, ok: bool) {
        bump(&self.sync_runs, if ok { "ok" } else { "error" });
    }

    pub fn gateway_cost(&self, micros: i64) {
        self.gateway_cost_micros
            .fetch_add(micros.max(0) as u64, Ordering::Relaxed);
    }

    /// Prometheus text exposition format.
    pub fn render(&self, pool: Option<(usize, usize)>) -> String {
        let mut o = String::new();
        let lock = |m: &Mutex<BTreeMap<&'static str, u64>>| {
            m.lock().unwrap_or_else(|p| p.into_inner()).clone()
        };
        o.push_str("# TYPE heed_http_requests_total counter\n");
        for ((route, status), n) in self
            .requests
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
        {
            let _ = writeln!(
                o,
                "heed_http_requests_total{{route=\"{}\",status=\"{status}\"}} {n}",
                route.replace(['"', '\\', '\n'], "_")
            );
        }
        o.push_str("# TYPE heed_http_request_duration_seconds histogram\n");
        for (b, n) in BUCKETS.iter().zip(&self.latency_buckets) {
            let _ = writeln!(
                o,
                "heed_http_request_duration_seconds_bucket{{le=\"{b}\"}} {}",
                n.load(Ordering::Relaxed)
            );
        }
        let count = self.latency_count.load(Ordering::Relaxed);
        let _ = writeln!(
            o,
            "heed_http_request_duration_seconds_bucket{{le=\"+Inf\"}} {count}\n\
             heed_http_request_duration_seconds_sum {}\n\
             heed_http_request_duration_seconds_count {count}",
            self.latency_sum_micros.load(Ordering::Relaxed) as f64 / 1e6
        );
        o.push_str("# TYPE heed_rate_limited_total counter\n");
        for (scope, n) in lock(&self.rate_limited) {
            let _ = writeln!(o, "heed_rate_limited_total{{scope=\"{scope}\"}} {n}");
        }
        o.push_str("# TYPE heed_sync_runs_total counter\n");
        for (result, n) in lock(&self.sync_runs) {
            let _ = writeln!(o, "heed_sync_runs_total{{result=\"{result}\"}} {n}");
        }
        let _ = writeln!(
            o,
            "# TYPE heed_gateway_cost_micros_total counter\nheed_gateway_cost_micros_total {}",
            self.gateway_cost_micros.load(Ordering::Relaxed)
        );
        if let Some((size, available)) = pool {
            let _ = writeln!(
                o,
                "# TYPE heed_db_pool_size gauge\nheed_db_pool_size {size}\n\
                 # TYPE heed_db_pool_available gauge\nheed_db_pool_available {available}"
            );
        }
        o
    }
}

// ---- error reports (Sentry-compatible) ----------------------------------------------------------

/// Where error reports go: a Sentry-compatible DSN `https://<key>@<host>/<project>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Dsn {
    pub key: String,
    pub envelope_url: String,
    raw: String,
}

impl Dsn {
    pub fn parse(raw: &str) -> Option<Self> {
        let u = url::Url::parse(raw.trim()).ok()?;
        if !matches!(u.scheme(), "https" | "http") || u.username().is_empty() {
            return None;
        }
        let project = u.path().trim_matches('/').rsplit('/').next()?.to_owned();
        if project.is_empty() || !project.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        let prefix = u
            .path()
            .trim_matches('/')
            .strip_suffix(project.as_str())
            .unwrap_or("")
            .trim_matches('/');
        let host = u.host_str()?;
        let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
        let prefix = if prefix.is_empty() {
            String::new()
        } else {
            format!("/{prefix}")
        };
        Some(Self {
            key: u.username().to_owned(),
            envelope_url: format!(
                "{}://{host}{port}{prefix}/api/{project}/envelope/",
                u.scheme()
            ),
            raw: raw.trim().to_owned(),
        })
    }
}

pub struct Reporter {
    http: reqwest::Client,
    dsn: Dsn,
    environment: String,
}

static REPORTER: OnceLock<Reporter> = OnceLock::new();

impl Reporter {
    pub fn new(http: reqwest::Client, dsn: Dsn, environment: String) -> Self {
        Self {
            http,
            dsn,
            environment,
        }
    }

    /// Installs the process-wide reporter (once; later calls are ignored).
    pub fn install(self) {
        let _ = REPORTER.set(self);
    }

    /// The envelope for one event. Only the given fields: no request data, no user.
    pub fn envelope(&self, message: &str, tags: &[(&str, &str)]) -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let tags: serde_json::Map<String, serde_json::Value> = tags
            .iter()
            .map(|(k, v)| ((*k).to_owned(), json!(v)))
            .collect();
        let event = json!({
            "event_id": id,
            "timestamp": time::OffsetDateTime::now_utc().unix_timestamp(),
            "platform": "other",
            "level": "error",
            "logger": "heed-cloud",
            "release": concat!("heed-cloud@", env!("CARGO_PKG_VERSION")),
            "environment": self.environment,
            "message": {"formatted": message},
            "tags": tags,
        });
        format!(
            "{}\n{}\n{}\n",
            json!({"event_id": id, "dsn": self.dsn.raw}),
            json!({"type": "event"}),
            event
        )
    }

    pub async fn send(&self, message: &str, tags: &[(&str, &str)]) {
        let auth = format!(
            "Sentry sentry_version=7, sentry_key={}, sentry_client=heed-cloud/{}",
            self.dsn.key,
            env!("CARGO_PKG_VERSION")
        );
        let res = self
            .http
            .post(&self.dsn.envelope_url)
            .header("x-sentry-auth", auth)
            .header("content-type", "application/x-sentry-envelope")
            .timeout(Duration::from_secs(5))
            .body(self.envelope(message, tags))
            .send()
            .await;
        if let Err(e) = res {
            tracing::debug!(error = %e, "error report not sent");
        }
    }
}

/// Reports an error in the background if a reporter is installed. `message` must not contain
/// personal data.
pub fn report(message: String, tags: Vec<(&'static str, String)>) {
    if REPORTER.get().is_none() {
        return;
    }
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        return;
    };
    rt.spawn(async move {
        if let Some(r) = REPORTER.get() {
            let t: Vec<(&str, &str)> = tags.iter().map(|(k, v)| (*k, v.as_str())).collect();
            r.send(&message, &t).await;
        }
    });
}

/// Logs panics as structured errors and reports them (the location only, not the payload,
/// which could hold data).
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let at = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        tracing::error!(location = %at, "panic");
        report(format!("panic at {at}"), vec![("kind", "panic".into())]);
        default(info);
    }));
}

// ---- middleware ---------------------------------------------------------------------------------

fn route_of(req: &Request) -> String {
    req.extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".into())
}

/// Request id, one log line, metrics and error reports for every request.
pub async fn observe(mut req: Request, next: Next) -> Response {
    let started = Instant::now();
    let rid = request_id(req.headers());
    let route = route_of(&req);
    let method = req.method().to_string();
    req.extensions_mut().insert(RequestId(rid.clone()));
    let span = tracing::info_span!("request", request_id = %rid);
    let mut res = next.run(req).instrument(span.clone()).await;
    let status = res.status().as_u16();
    let elapsed = started.elapsed();
    if let Ok(v) = HeaderValue::from_str(&rid) {
        res.headers_mut().insert("x-request-id", v);
    }
    METRICS.record_request(&route, status, elapsed);
    let ms = elapsed.as_millis() as u64;
    span.in_scope(|| {
        if status >= 500 {
            tracing::warn!(%method, %route, status, latency_ms = ms, "request");
        } else if !matches!(route.as_str(), "/healthz" | "/readyz") {
            tracing::info!(%method, %route, status, latency_ms = ms, "request");
        }
    });
    if status >= 500 {
        report(
            format!("{method} {route} answered {status}"),
            vec![
                ("route", route),
                ("status", status.to_string()),
                ("request_id", rid),
            ],
        );
    }
    res
}

/// Per-request deadline: Ask may take a minute (model latency), everything else 30 seconds.
pub async fn deadline(req: Request, next: Next) -> Response {
    // Prefix match so a streaming Ask route (/v1/ask/...) gets the longer deadline too. The
    // deadline covers producing the response head; a streamed body must bound itself.
    let limit = if route_of(&req).starts_with("/v1/ask") {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(30)
    };
    match tokio::time::timeout(limit, next.run(req)).await {
        Ok(res) => res,
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            axum::Json(json!({"error": "timeout", "message": "the request took too long"})),
        )
            .into_response(),
    }
}

// ---- shutdown -----------------------------------------------------------------------------------

/// Resolves on SIGTERM (Fly stopping the machine) or Ctrl-C.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn request_ids_are_plain_or_fresh() {
        let mut h = HeaderMap::new();
        h.insert("x-request-id", "abc-123_456.7".parse().unwrap());
        assert_eq!(request_id(&h), "abc-123_456.7");
        h.insert("x-request-id", "short".parse().unwrap());
        h.insert("fly-request-id", "01HXYZFLYREQUEST-lhr".parse().unwrap());
        assert_eq!(
            request_id(&h),
            "01HXYZFLYREQUEST-lhr",
            "falls back to Fly's"
        );
        let mut h = HeaderMap::new();
        h.insert("x-request-id", "evil\" injected=1 aaaa".parse().unwrap());
        let id = request_id(&h);
        assert_eq!(id.len(), 32, "replaced: {id}");
    }

    #[test]
    fn dsn_parses_and_rejects() {
        let d = Dsn::parse("https://pub123@o1.ingest.sentry.io/4505").unwrap();
        assert_eq!(d.key, "pub123");
        assert_eq!(
            d.envelope_url,
            "https://o1.ingest.sentry.io/api/4505/envelope/"
        );
        let d = Dsn::parse("http://k@localhost:9000/sub/7").unwrap();
        assert_eq!(d.envelope_url, "http://localhost:9000/sub/api/7/envelope/");
        assert!(
            Dsn::parse("https://o1.ingest.sentry.io/4505").is_none(),
            "no key"
        );
        assert!(Dsn::parse("ftp://k@h/1").is_none());
        assert!(Dsn::parse("https://k@h/").is_none());
    }

    #[tokio::test]
    async fn reports_go_to_the_envelope_endpoint_without_payloads() {
        use wiremock::matchers::{header_regex, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/42/envelope/"))
            .and(header_regex("x-sentry-auth", "sentry_key=k1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let dsn = Dsn::parse(&format!("{}/42", server.uri().replace("://", "://k1@"))).unwrap();
        let r = Reporter::new(reqwest::Client::new(), dsn, "test".into());
        r.send("POST /v1/ask answered 500", &[("route", "/v1/ask")])
            .await;
        let body =
            String::from_utf8(server.received_requests().await.unwrap()[0].body.clone()).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3);
        let event: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(event["tags"]["route"], "/v1/ask");
        assert!(event.get("user").is_none() && event.get("request").is_none());
    }

    #[test]
    fn metrics_render() {
        let m = &*METRICS;
        m.record_request("/v1/ask", 200, Duration::from_millis(30));
        m.rate_limited("ip");
        m.sync_run(true);
        m.gateway_cost(1040);
        let text = m.render(Some((8, 7)));
        assert!(text.contains("heed_http_requests_total{route=\"/v1/ask\",status=\"200\"}"));
        assert!(text.contains("heed_http_request_duration_seconds_bucket{le=\"+Inf\"}"));
        assert!(text.contains("heed_rate_limited_total{scope=\"ip\"}"));
        assert!(text.contains("heed_db_pool_available 7"));
    }
}
