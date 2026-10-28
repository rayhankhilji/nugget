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
use hyper_util::rt::TokioIo;
use nugget_core::{Error, Result};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::auth::ClientRegistry;
use crate::server::{AuthenticatedClient, McpHandler, McpServer};

/// The MCP endpoint path.
pub const MCP_PATH: &str = "/mcp";
/// Default loopback port.
pub const DEFAULT_PORT: u16 = 7466;
const MAX_BODY_BYTES: usize = 1024 * 1024;

type Body = BoxBody<Bytes, Infallible>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpConfig {
    /// Port on 127.0.0.1; 0 picks a free one.
    pub port: u16,
    /// Exact `Origin` values allowed (e.g. `http://localhost:6274` for the MCP Inspector).
    /// Empty: every request carrying an `Origin` header is refused.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            allowed_origins: Vec::new(),
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
        });
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            loop {
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
                    let svc = hyper::service::service_fn(move |req| {
                        let gate = gate.clone();
                        async move { Ok::<_, Infallible>(gate.handle(req).await) }
                    });
                    let conn = hyper::server::conn::http1::Builder::new()
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

impl Gate {
    async fn handle(&self, mut req: Request<Incoming>) -> Response<Body> {
        if req.uri().path() != MCP_PATH {
            return plain(StatusCode::NOT_FOUND, "not found");
        }
        if req.uri().query().is_some() {
            return plain(
                StatusCode::BAD_REQUEST,
                "query strings are not accepted; send the token in the Authorization header",
            );
        }
        let host_ok = req
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| self.allowed_hosts.iter().any(|a| a.eq_ignore_ascii_case(h)));
        if !host_ok {
            return plain(StatusCode::FORBIDDEN, "forbidden host");
        }
        if let Some(origin) = req.headers().get(header::ORIGIN) {
            let ok = origin
                .to_str()
                .is_ok_and(|o| self.allowed_origins.iter().any(|a| a == o));
            if !ok {
                return plain(StatusCode::FORBIDDEN, "forbidden origin");
            }
        }
        if req.method() == Method::OPTIONS {
            return plain(
                StatusCode::FORBIDDEN,
                "cross-origin requests are not allowed",
            );
        }
        let token = req
            .headers()
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
                return plain(StatusCode::SERVICE_UNAVAILABLE, "try again later");
            }
            _ => {
                let mut r = plain(StatusCode::UNAUTHORIZED, "missing or invalid bearer token");
                r.headers_mut().insert(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Bearer realm=\"nugget\""),
                );
                return r;
            }
        };
        if let Err(retry) = self.server.limiter().check(&client.id) {
            let mut r = plain(StatusCode::TOO_MANY_REQUESTS, "rate limited");
            r.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from(retry.as_secs().max(1)),
            );
            return r;
        }
        req.extensions_mut().insert(AuthenticatedClient(client));
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
}
