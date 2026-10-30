//! Streamable HTTP transport on 127.0.0.1.
//!
//! Every request passes these gates, in order, before it reaches the MCP service:
//! 1. **Loopback peer** only (the listener binds 127.0.0.1; this is defence in depth).
//! 2. **Path** `/mcp`, and **no query string at all**: credentials never travel in URLs.
//! 3. **`Host`** must be `127.0.0.1:<port>` or `localhost:<port>` (DNS-rebinding protection).
//! 4. **`Origin`**, when present, must be on the configured allow-list (empty by default, so any
//!    browser page is refused). **CORS is closed**: preflights get 403 and no response ever
//!    carries `Access-Control-*` headers.
//! 5. **`Authorization: Bearer ngt_…`** must name a live client (401 otherwise).
//! 6. **Per-client rate limit** (429 with `Retry-After`).
//!
//! Refusals from these gates (and the body limits below) keep their HTTP status and headers and
//! carry a JSON-RPC error body (`application/json`, `id: null`), so MCP SDK clients surface the
//! reason ("missing or invalid bearer token", "rate limited; retry after 3 s") instead of a bare
//! status. Only an unknown path gets a plain 404.
//!
//! Resource limits: the listener binds exactly the configured port and fails if it is taken (no
//! fallback, so a squatter is never mistaken for Nugget); at most
//! [`HttpConfig::max_connections`] connections are served at once (others wait in the backlog);
//! a connection that does not send a complete request head within
//! [`HttpConfig::header_timeout_ms`] (also while idle between requests) is closed; a body not
//! fully received within [`HttpConfig::body_timeout_ms`] gets 408, and one over 1 MiB gets 413.
//!
//! The MCP layer is rmcp's `StreamableHttpService`, stateless (no sessions: each request
//! carries its own credentials and scopes) with JSON responses, and its own Host/Origin checks
//! configured to match.

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use nugget_core::{Error, Result};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use nugget_core::ipc::McpClientInfo;

use crate::auth::ClientRegistry;
use crate::server::{AuthenticatedClient, McpHandler, McpServer};

/// The MCP endpoint path.
pub const MCP_PATH: &str = "/mcp";
/// Default loopback port (the daemon's `[mcp] port` default).
pub const DEFAULT_PORT: u16 = 7319;
/// Default cap on open connections.
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;
/// Default time a connection gets to send a request's headers (also the idle keep-alive limit).
pub const DEFAULT_HEADER_TIMEOUT_MS: u64 = 10_000;
/// Default time a request gets to send its whole body.
pub const DEFAULT_BODY_TIMEOUT_MS: u64 = 30_000;
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Most bytes of a refused request's body read (and discarded) before answering.
const DRAIN_BYTES: usize = 64 * 1024;
/// Longest wait for a refused request's body.
const DRAIN_TIMEOUT_MS: u64 = 2_000;

type Body = BoxBody<Bytes, Infallible>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpConfig {
    /// Port on 127.0.0.1; 0 picks a free one. Binding fails (no fallback port) when it is taken.
    pub port: u16,
    /// Exact `Origin` values allowed (e.g. `http://localhost:6274` for the MCP Inspector).
    /// Empty: every request carrying an `Origin` header is refused.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Most connections served at once; further ones wait in the listen backlog. At least 1.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Milliseconds a connection gets to send a complete request head, counted from when the
    /// server starts waiting for it (so an idle keep-alive connection is closed after this too).
    #[serde(default = "default_header_timeout_ms")]
    pub header_timeout_ms: u64,
    /// Milliseconds a request gets to send its whole body once the head is in (408 otherwise).
    #[serde(default = "default_body_timeout_ms")]
    pub body_timeout_ms: u64,
}

fn default_max_connections() -> usize {
    DEFAULT_MAX_CONNECTIONS
}
fn default_header_timeout_ms() -> u64 {
    DEFAULT_HEADER_TIMEOUT_MS
}
fn default_body_timeout_ms() -> u64 {
    DEFAULT_BODY_TIMEOUT_MS
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            allowed_origins: Vec::new(),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            header_timeout_ms: DEFAULT_HEADER_TIMEOUT_MS,
            body_timeout_ms: DEFAULT_BODY_TIMEOUT_MS,
        }
    }
}

/// A running HTTP listener.
#[derive(Debug)]
pub struct HttpHandle {
    addr: SocketAddr,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl HttpHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>/mcp`
    pub fn url(&self) -> String {
        format!("http://{}{MCP_PATH}", self.addr)
    }

    /// Stop accepting connections and end open ones.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }
}

struct Gate {
    server: McpServer,
    registry: Arc<ClientRegistry>,
    service: StreamableHttpService<McpHandler, NeverSessionManager>,
    allowed_hosts: [String; 2],
    allowed_origins: Vec<String>,
    body_timeout: Duration,
}

impl McpServer {
    /// Serve streamable HTTP on `127.0.0.1:<port>`. Needs the server's client registry.
    pub async fn serve_http(&self, config: HttpConfig) -> Result<HttpHandle> {
        let registry = self.registry().cloned().ok_or_else(|| {
            Error::Invalid("serving MCP over HTTP needs a client registry".into())
        })?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port)).await?;
        let addr = listener.local_addr()?;
        let cancel = CancellationToken::new();
        let allowed_hosts = [
            format!("127.0.0.1:{}", addr.port()),
            format!("localhost:{}", addr.port()),
        ];
        let mut rmcp_config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_allowed_hosts(allowed_hosts.clone())
            .with_allowed_origins(config.allowed_origins.clone())
            .enforce_origin_validation()
            .with_max_request_body_bytes(MAX_BODY_BYTES)
            .with_cancellation_token(cancel.child_token());
        rmcp_config.sse_keep_alive = Some(Duration::from_secs(15));
        let factory_server = self.clone();
        let service = StreamableHttpService::new(
            move || Ok(factory_server.http_handler()),
            Arc::new(NeverSessionManager::default()),
            rmcp_config,
        );
        let gate = Arc::new(Gate {
            server: self.clone(),
            registry,
            service,
            allowed_hosts,
            allowed_origins: config.allowed_origins,
            body_timeout: Duration::from_millis(config.body_timeout_ms),
        });
        let header_timeout = Duration::from_millis(config.header_timeout_ms);
        let slots = Arc::new(Semaphore::new(config.max_connections.max(1)));
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            loop {
                // Take a slot before accepting: over the cap, connections wait in the backlog.
                let permit = tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    p = slots.clone().acquire_owned() => match p {
                        Ok(p) => p,
                        Err(_) => break,
                    },
                };
                let (stream, peer) = tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    r = listener.accept() => match r {
                        Ok(x) => x,
                        Err(e) => {
                            tracing::warn!(error = %e, "mcp http accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                    },
                };
                if !peer.ip().is_loopback() {
                    continue;
                }
                let gate = gate.clone();
                let conn_cancel = task_cancel.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let svc = hyper::service::service_fn(move |req| {
                        let gate = gate.clone();
                        async move { Ok::<_, Infallible>(gate.handle(req).await) }
                    });
                    let conn = hyper::server::conn::http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(header_timeout)
                        .serve_connection(TokioIo::new(stream), svc);
                    tokio::select! {
                        r = conn => if let Err(e) = r {
                            tracing::debug!(error = %e, "mcp http connection ended");
                        },
                        _ = conn_cancel.cancelled() => {}
                    }
                });
            }
        });
        Ok(HttpHandle { addr, cancel, task })
    }
}

fn plain(status: StatusCode, msg: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Full::new(Bytes::from_static(msg.as_bytes())).boxed())
        .expect("valid response")
}

/// JSON-RPC "Invalid Request".
const INVALID_REQUEST: i32 = -32600;
/// JSON-RPC "Internal error".
const INTERNAL_ERROR: i32 = -32603;

/// A refusal with a JSON-RPC error body, so SDK clients can show `msg`. The request is refused
/// before its body is parsed, so the id is `null`.
fn refuse(status: StatusCode, msg: impl Into<String>) -> Response<Body> {
    let code = if status.is_server_error() {
        INTERNAL_ERROR
    } else {
        INVALID_REQUEST
    };
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {"code": code, "message": msg.into(), "data": {"http_status": status.as_u16()}},
    });
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Full::new(Bytes::from(body.to_string())).boxed())
        .expect("valid response")
}

impl Gate {
    async fn handle(&self, req: Request<Incoming>) -> Response<Body> {
        let (mut parts, body) = req.into_parts();
        let client = match self.admit(&parts) {
            Ok(c) => c,
            Err(refusal) => return self.refused(body, *refusal).await,
        };
        // Read the whole body under a deadline and a size cap, so a slow sender cannot hold a
        // connection slot.
        let limited = http_body_util::Limited::new(body, MAX_BODY_BYTES);
        let bytes = match tokio::time::timeout(self.body_timeout, limited.collect()).await {
            Err(_) => return refuse(StatusCode::REQUEST_TIMEOUT, "request body timed out"),
            Ok(Err(e)) if e.is::<http_body_util::LengthLimitError>() => {
                return refuse(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
            }
            Ok(Err(_)) => return refuse(StatusCode::BAD_REQUEST, "unreadable request body"),
            Ok(Ok(c)) => c.to_bytes(),
        };
        parts.extensions.insert(AuthenticatedClient(client));
        let req = Request::from_parts(parts, Full::new(bytes));
        let mut svc = self.service.clone();
        let mut resp = match tower_service::Service::call(&mut svc, req).await {
            Ok(r) => r,
            Err(never) => match never {},
        };
        let cors: Vec<_> = resp
            .headers()
            .keys()
            .filter(|k| k.as_str().starts_with("access-control-"))
            .cloned()
            .collect();
        for k in cors {
            resp.headers_mut().remove(k);
        }
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        resp
    }

    /// Finish a refused request: read (and discard) a small body first. Closing a connection with
    /// unread request bytes makes the OS reset it, and the client then loses the refusal (an SDK
    /// reports "server disconnected" instead of "rate limited"). A body that is large or slow is
    /// not waited for; the connection is closed instead.
    async fn refused(&self, body: Incoming, mut refusal: Response<Body>) -> Response<Body> {
        let wait = self
            .body_timeout
            .min(Duration::from_millis(DRAIN_TIMEOUT_MS));
        let drain = http_body_util::Limited::new(body, DRAIN_BYTES).collect();
        let drained = matches!(tokio::time::timeout(wait, drain).await, Ok(Ok(_)));
        if !drained {
            refusal
                .headers_mut()
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
        }
        refusal
    }

    /// The gates before the body: path, query, Host, Origin, CORS, bearer token, rate limit.
    fn admit(&self, req: &http::request::Parts) -> Result<McpClientInfo, Box<Response<Body>>> {
        if req.uri.path() != MCP_PATH {
            return Err(Box::new(plain(StatusCode::NOT_FOUND, "not found")));
        }
        if req.uri.query().is_some() {
            return Err(Box::new(refuse(
                StatusCode::BAD_REQUEST,
                "query strings are not accepted; send the token in the Authorization header",
            )));
        }
        let host_ok = req
            .headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| self.allowed_hosts.iter().any(|a| a.eq_ignore_ascii_case(h)));
        if !host_ok {
            return Err(Box::new(refuse(StatusCode::FORBIDDEN, "forbidden host")));
        }
        if let Some(origin) = req.headers.get(header::ORIGIN) {
            let ok = origin
                .to_str()
                .is_ok_and(|o| self.allowed_origins.iter().any(|a| a == o));
            if !ok {
                return Err(Box::new(refuse(StatusCode::FORBIDDEN, "forbidden origin")));
            }
        }
        if req.method == Method::OPTIONS {
            return Err(Box::new(refuse(
                StatusCode::FORBIDDEN,
                "cross-origin requests are not allowed",
            )));
        }
        let token = req
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                let (scheme, rest) = v.split_once(' ')?;
                scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
            });
        let client = match token.map(|t| self.registry.authenticate(t)) {
            Some(Ok(Some(c))) => c,
            Some(Err(e)) => {
                tracing::warn!(error = %e, "mcp client lookup failed");
                return Err(Box::new(refuse(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "try again later",
                )));
            }
            _ => {
                let mut r = refuse(
                    StatusCode::UNAUTHORIZED,
                    "missing or invalid bearer token (or the MCP client was revoked in Nugget)",
                );
                r.headers_mut().insert(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Bearer realm=\"nugget\""),
                );
                return Err(Box::new(r));
            }
        };
        if let Err(retry) = self.server.limiter().check(&client.id) {
            let secs = retry.as_secs().max(1);
            let mut r = refuse(
                StatusCode::TOO_MANY_REQUESTS,
                format!("rate limited; retry after {secs} s"),
            );
            r.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
            return Err(Box::new(r));
        }
        Ok(client)
    }
}
