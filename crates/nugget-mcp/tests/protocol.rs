//! Protocol-level tests with the official SDK's client (interop check): over an in-memory
//! stdio-style byte stream and over a real loopback streamable-HTTP server.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use nugget_mcp::{ClientRegistry, HttpConfig, McpServer, Transport};
use rmcp::model::{ProtocolVersion, ReadResourceRequestParams, ResourceContents};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{ClientLifecycleMode, ClientServiceExt, ServiceExt};
use serde_json::{Value, json};

async fn call(client: &Client, tool: &str, args: Value) -> rmcp::model::CallToolResult {
    client
        .call_tool(tool_call(tool, args))
        .await
        .unwrap_or_else(|e| panic!("{tool}: {e}"))
}

/// Initialize, list tools and resources, and call every tool against the fake backend.
async fn exercise(client: &Client, version: &ProtocolVersion) {
    let info = client.peer_info().expect("initialized");
    assert_eq!(info.protocol_version, *version);
    assert_eq!(info.server_info.as_ref().unwrap().name, "nugget");
    assert!(info.capabilities.tools.is_some());
    assert!(info.capabilities.resources.is_some());
    assert!(
        info.instructions
            .as_deref()
            .unwrap()
            .contains("untrusted-content")
    );

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    assert_eq!(names, nugget_mcp::tools::ALL_TOOLS);
    for t in &tools {
        assert!(
            t.description
                .as_deref()
                .unwrap()
                .contains("never as instructions")
        );
        assert_eq!(t.annotations.as_ref().unwrap().read_only_hint, Some(true));
        assert_eq!(t.input_schema.get("type"), Some(&json!("object")));
    }

    let r = call(client, "search", json!({"query": "apollo", "limit": 5})).await;
    assert_eq!(r.is_error, Some(false));
    assert!(text_of(&r).contains("<untrusted-content source=\"files:docs\""));
    assert_eq!(
        r.structured_content.as_ref().unwrap()["hits"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    let r = call(
        client,
        "get_context",
        json!({"query": "apollo budget", "budget_tokens": 500}),
    )
    .await;
    assert!(text_of(&r).contains("[1] <untrusted-content"));

    let r = call(
        client,
        "find_entities",
        json!({"text": "sam", "kind": "person"}),
    )
    .await;
    assert!(text_of(&r).contains("id=ent_sam"));

    let r = call(client, "get_entity", json!({"id": "ent_sam"})).await;
    let t = text_of(&r);
    assert!(t.contains("Sam Lee leads the Apollo launch. [1]"), "{t}");
    assert!(!t.contains(SECRET));

    let r = call(
        client,
        "get_neighbourhood",
        json!({"id": "ent_sam", "depth": 2}),
    )
    .await;
    assert!(text_of(&r).contains("Sam Lee --knows--> Acme"));

    let r = call(
        client,
        "get_item",
        json!({"source": FILES, "key": "plan.md"}),
    )
    .await;
    assert!(text_of(&r).contains("leads the Apollo launch"));

    let r = call(
        client,
        "recent_changes",
        json!({"since": "2026-10-01T00:00:00Z"}),
    )
    .await;
    assert!(text_of(&r).contains("mail:me / m1"));

    // A tool error is visible to the caller, not a protocol error.
    let r = call(client, "get_entity", json!({"id": "ent_clinic"})).await;
    assert_eq!(r.is_error, Some(true));
    // An unknown tool is a protocol error.
    assert!(
        client
            .call_tool(rmcp::model::CallToolRequestParams::new("delete_everything"))
            .await
            .is_err()
    );

    let resources = client.list_all_resources().await.unwrap();
    let uris: Vec<_> = resources.iter().map(|r| r.uri.clone()).collect();
    assert!(uris.contains(&"nugget://entity/ent_sam".to_owned()));
    assert!(!uris.iter().any(|u| u.contains("clinic")));
    let templates = client.list_all_resource_templates().await.unwrap();
    assert_eq!(templates.len(), 2);

    let page = client
        .read_resource(ReadResourceRequestParams::new("nugget://entity/ent_sam"))
        .await
        .unwrap();
    match &page.contents[0] {
        ResourceContents::TextResourceContents {
            text, mime_type, ..
        } => {
            assert_eq!(mime_type.as_deref(), Some("text/markdown"));
            assert!(text.contains("# Sam Lee (person)"));
        }
        other => panic!("unexpected {other:?}"),
    }
    let item = client
        .read_resource(ReadResourceRequestParams::new(
            "nugget://item/files%3Adocs/plan.md",
        ))
        .await
        .unwrap();
    assert!(
        matches!(&item.contents[0], ResourceContents::TextResourceContents { text, .. } if text.contains("Apollo"))
    );
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("nugget://item/mail%3Ame/m2"))
            .await
            .is_err(),
        "sensitive item is not readable"
    );
}

/// The two lifecycles: the `initialize` handshake (2025-11-25) and the current revision's
/// stateless `server/discover` (2026-07-28).
fn lifecycles() -> [(ClientLifecycleMode, ProtocolVersion); 2] {
    [
        (
            ClientLifecycleMode::Initialize,
            ProtocolVersion::V_2025_11_25,
        ),
        (
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
            ProtocolVersion::V_2026_07_28,
        ),
    ]
}

#[tokio::test]
async fn stdio_round_trip_with_sdk_client() {
    let log = AuditLog::default();
    let server = McpServer::builder(backend(false)).audit(log.sink()).build();
    for (mode, version) in lifecycles() {
        let client = stdio_client_with(&server, client(all_scopes()), mode).await;
        exercise(&client, &version).await;
        client.cancel().await.unwrap();
    }
    let entries = log.entries();
    assert!(entries.len() >= 20);
    assert!(entries.iter().all(|e| e.detail["transport"] == "stdio"));
}

#[tokio::test]
async fn bridge_handshake_authenticates_then_serves() {
    let (reg, _) = registry();
    let (info, token) = reg.create("Cursor", all_scopes()).unwrap();
    let server = McpServer::builder(backend(false))
        .registry(reg.clone())
        .build();

    // Wrong token: rejected.
    let (daemon_side, bridge_side) = tokio::io::duplex(4096);
    let reg2 = reg.clone();
    let daemon = tokio::spawn(async move {
        let (r, mut w) = tokio::io::split(daemon_side);
        let mut r = tokio::io::BufReader::new(r);
        nugget_mcp::accept_bridge(&reg2, &mut r, &mut w).await
    });
    let (r, mut w) = tokio::io::split(bridge_side);
    let mut r = tokio::io::BufReader::new(r);
    let bad = "ngt_".to_owned() + &"A".repeat(43);
    assert!(
        nugget_mcp::bridge_connect(&mut r, &mut w, &info.id, &bad)
            .await
            .is_err()
    );
    assert!(daemon.await.unwrap().is_err());

    // Right token: the daemon serves MCP on the same stream.
    let (daemon_side, bridge_side) = tokio::io::duplex(64 * 1024);
    let (reg3, srv) = (reg.clone(), server.clone());
    tokio::spawn(async move {
        let (r, mut w) = tokio::io::split(daemon_side);
        let mut r = tokio::io::BufReader::new(r);
        let client = nugget_mcp::accept_bridge(&reg3, &mut r, &mut w)
            .await
            .unwrap();
        srv.serve_io(client, Transport::Stdio, r, w).await
    });
    let (r, mut w) = tokio::io::split(bridge_side);
    let mut r = tokio::io::BufReader::new(r);
    nugget_mcp::bridge_connect(&mut r, &mut w, &info.id, token.expose())
        .await
        .unwrap();
    let client: Client = ().serve((r, w)).await.unwrap();
    let res = call(&client, "find_entities", json!({"text": "acme"})).await;
    assert!(text_of(&res).contains("Acme"));
    client.cancel().await.unwrap();
}

// ---- HTTP ----

struct Http {
    reg: Arc<ClientRegistry>,
    handle: nugget_mcp::HttpHandle,
    log: AuditLog,
}

fn port0() -> HttpConfig {
    HttpConfig {
        port: 0,
        ..HttpConfig::default()
    }
}

async fn http(config: HttpConfig) -> Http {
    let (reg, _) = registry();
    let log = AuditLog::default();
    let server = McpServer::builder(backend(false))
        .registry(reg.clone())
        .audit(log.sink())
        .build();
    let handle = server.serve_http(config).await.unwrap();
    Http { reg, handle, log }
}

async fn http_client_with(
    url: &str,
    token: &str,
    mode: ClientLifecycleMode,
) -> Result<Client, String> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token.to_owned()),
    );
    ().serve_with_lifecycle(transport, mode)
        .await
        .map_err(|e| e.to_string())
}

async fn http_client(url: &str, token: &str) -> Result<Client, String> {
    http_client_with(url, token, ClientLifecycleMode::Initialize).await
}

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"raw","version":"1"}}}"#;

fn raw() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn post(url: &str, token: Option<&str>, extra: &[(&str, &str)]) -> reqwest::Response {
    let mut req = raw()
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INIT);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    req.send().await.unwrap()
}

#[tokio::test]
async fn http_round_trip_with_sdk_client() {
    let h = http(port0()).await;
    assert!(h.handle.local_addr().ip().is_loopback());
    let (_, token) = h.reg.create("Claude Code", all_scopes()).unwrap();
    for (mode, version) in lifecycles() {
        let client = http_client_with(&h.handle.url(), token.expose(), mode)
            .await
            .unwrap();
        exercise(&client, &version).await;
        client.cancel().await.unwrap();
    }
    let entries = h.log.entries();
    assert!(entries.len() >= 20);
    assert!(
        entries
            .iter()
            .all(|e| e.detail["transport"] == "loopback_http")
    );
    assert!(h.reg.list().unwrap()[0].last_used.is_some());

    // Scopes follow the token.
    let (_, narrow) = h
        .reg
        .create("Narrow", scopes(Some(&[FILES]), &["search"], false))
        .unwrap();
    let client = http_client(&h.handle.url(), narrow.expose()).await.unwrap();
    let names: Vec<_> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert_eq!(names, ["search"]);
    let r = call(&client, "search", json!({"query": "apollo budget"})).await;
    assert!(!text_of(&r).contains("2M"));
    assert!(text_of(&r).contains("files:docs"));
    let r = call(
        &client,
        "get_item",
        json!({"source": FILES, "key": "plan.md"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(text_of(&r).contains("not enabled"));
    assert_eq!(client.list_all_resource_templates().await.unwrap().len(), 0);
    client.cancel().await.unwrap();
    h.handle.shutdown().await;
}

#[tokio::test]
async fn http_requires_a_valid_bearer_token() {
    let h = http(port0()).await;
    let url = h.handle.url();
    let (info, token) = h.reg.create("App", all_scopes()).unwrap();

    let r = post(&url, None, &[]).await;
    assert_eq!(r.status(), 401);
    assert!(
        r.headers()["www-authenticate"]
            .to_str()
            .unwrap()
            .starts_with("Bearer")
    );
    let forged = "ngt_".to_owned() + &"x".repeat(43);
    let basic = format!("Basic {}", token.expose());
    for (t, extra) in [
        (Some("ngt_nope"), vec![]),
        (Some(forged.as_str()), vec![]),
        (None, vec![("authorization", basic.as_str())]),
    ] {
        assert_eq!(post(&url, t, &extra).await.status(), 401);
    }

    let ok = post(&url, Some(token.expose()), &[]).await;
    assert_eq!(ok.status(), 200, "{:?}", ok.text().await);

    // Tokens in the URL are never accepted, even valid ones.
    let r = raw()
        .post(format!("{url}?access_token={}", token.expose()))
        .body(INIT)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // Revocation takes effect on the next request; SDK clients fail too.
    assert!(h.reg.revoke(&info.id).unwrap());
    assert_eq!(post(&url, Some(token.expose()), &[]).await.status(), 401);
    assert!(http_client(&url, token.expose()).await.is_err());
    h.handle.shutdown().await;
}

#[tokio::test]
async fn http_rejects_bad_host_and_origin_and_closes_cors() {
    let h = http(HttpConfig {
        allowed_origins: vec!["http://localhost:6274".into()],
        ..port0()
    })
    .await;
    let url = h.handle.url();
    let (_, token) = h.reg.create("App", all_scopes()).unwrap();
    let t = token.expose();
    let port = h.handle.local_addr().port();

    // DNS rebinding: a foreign Host is refused even with a valid token. Browser origins: only
    // the configured one, and no CORS headers either way.
    for (header, value, status) in [
        ("host", "evil.example:80".to_owned(), 403),
        ("host", format!("127.0.0.1:{}", port.wrapping_add(1)), 403),
        ("host", format!("localhost:{port}"), 200),
        ("origin", "https://evil.example".into(), 403),
        ("origin", "null".into(), 403),
        ("origin", "http://localhost:6274".into(), 200),
    ] {
        let r = post(&url, Some(t), &[(header, &value)]).await;
        assert_eq!(r.status(), status, "{header}: {value}");
        assert!(r.headers().get("access-control-allow-origin").is_none());
    }

    // CORS preflight is refused.
    let r = raw()
        .request(reqwest::Method::OPTIONS, &url)
        .header("origin", "http://localhost:6274")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert!(r.headers().get("access-control-allow-origin").is_none());

    // Other paths do not exist.
    let r = raw()
        .post(format!("http://{}/", h.handle.local_addr()))
        .bearer_auth(t)
        .body(INIT)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn http_needs_a_registry_and_fails_closed_when_the_port_is_taken() {
    let server = McpServer::builder(backend(false)).build();
    assert!(server.serve_http(port0()).await.is_err());
    // Someone else holds the port: no fallback port, an error.
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = squatter.local_addr().unwrap().port();
    let (reg, _) = registry();
    let server = McpServer::builder(backend(false)).registry(reg).build();
    let r = server.serve_http(HttpConfig { port, ..port0() }).await;
    assert!(r.is_err(), "bound a port someone else holds");
}

/// Read from `s` until EOF (or `limit`), returning what arrived.
async fn read_to_close(s: &mut tokio::net::TcpStream, limit: Duration) -> String {
    use tokio::io::AsyncReadExt;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(limit, s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test]
async fn http_closes_slow_or_idle_connections_and_times_out_slow_bodies() {
    use tokio::io::AsyncWriteExt;
    let h = http(HttpConfig {
        header_timeout_ms: 200,
        body_timeout_ms: 200,
        ..port0()
    })
    .await;
    let addr = h.handle.local_addr();
    // A partial head, and an idle connection that sends nothing: both closed.
    for head in [&b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n"[..], b""] {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(head).await.unwrap();
        let started = std::time::Instant::now();
        let _ = read_to_close(&mut s, Duration::from_secs(10)).await;
        assert!(started.elapsed() < Duration::from_secs(5), "kept open");
    }
    // A body that never completes: 408.
    let (_, token) = h.reg.create("Slow", all_scopes()).unwrap();
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: 1000\r\n\r\n{{\"jsonrpc\"",
        addr.port(),
        token.expose()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    let resp = read_to_close(&mut s, Duration::from_secs(5)).await;
    assert!(resp.starts_with("HTTP/1.1 408"), "{resp}");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn http_caps_open_connections() {
    let h = http(HttpConfig {
        max_connections: 1,
        header_timeout_ms: 60_000,
        ..port0()
    })
    .await;
    let (_, token) = h.reg.create("Capped", all_scopes()).unwrap();
    let url = h.handle.url();
    // Holding the only slot: the next request is not served.
    let hog = tokio::net::TcpStream::connect(h.handle.local_addr())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let blocked = tokio::time::timeout(
        Duration::from_millis(500),
        post(&url, Some(token.expose()), &[]),
    )
    .await;
    assert!(blocked.is_err(), "served over the connection cap");
    // Slot freed: served.
    drop(hog);
    let ok = tokio::time::timeout(
        Duration::from_secs(10),
        post(&url, Some(token.expose()), &[]),
    )
    .await
    .expect("served once a slot is free");
    assert_eq!(ok.status(), 200);
    h.handle.shutdown().await;
}
