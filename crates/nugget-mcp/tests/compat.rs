//! Fixes found by running the official TypeScript and Python SDK clients against Nugget
//! (`crates/nugget-mcp/compat`): every tool call and resource read that runs is audited exactly
//! once (unknown tools and cancelled requests too, including a cancellation racing completion),
//! `resources/list` refuses cursors it never handed out, cancelled requests stop, and HTTP
//! refusals carry a JSON-RPC error body that SDK clients can show.
//!
//! Refused calls (rate limited, revoked) are audited coalesced: at most one entry per client and
//! outcome per `REFUSAL_AUDIT_WINDOW`, counting the refusals in between. On stdio the entry names
//! the refused tool or resource. Over HTTP a rate-limited request is refused before its body is
//! read, so its entry is `mcp.request_refused` with target `http`; a request with a missing,
//! wrong or revoked token names no client and is not audited (it gets 401).

mod common;

use common::*;
use nugget_mcp::{HttpConfig, McpConfig, McpServer, RateLimit, Transport};
use rmcp::model::{CallToolRequestParams, PaginatedRequestParams, ReadResourceRequestParams};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ErrorData, ServiceError, ServiceExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

type Client = RunningService<RoleClient, ()>;

async fn stdio_client(server: &McpServer) -> Client {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let (r, w) = tokio::io::split(server_io);
    let s = server.clone();
    tokio::spawn(async move {
        s.serve_io(client(all_scopes()), Transport::Stdio, r, w)
            .await
    });
    ().serve(client_io).await.expect("client connects")
}

fn mcp_error(e: ServiceError) -> ErrorData {
    match e {
        ServiceError::McpError(e) => e,
        other => panic!("expected an MCP error, got {other}"),
    }
}

#[tokio::test]
async fn every_call_and_read_is_audited_once_including_unknown_tools() {
    let log = AuditLog::default();
    let server = McpServer::builder(backend(false)).audit(log.sink()).build();
    let client = stdio_client(&server).await;

    let ok = client
        .call_tool(
            CallToolRequestParams::new("search").with_arguments(obj(json!({"query": "apollo"}))),
        )
        .await
        .unwrap();
    assert_ne!(ok.is_error, Some(true));
    let bad = client
        .call_tool(CallToolRequestParams::new("search").with_arguments(obj(json!({"limit": 3}))))
        .await
        .unwrap();
    assert_eq!(
        bad.is_error,
        Some(true),
        "invalid arguments are a tool error"
    );
    let unknown = client
        .call_tool(CallToolRequestParams::new("rm_rf"))
        .await
        .map(|_| ())
        .map_err(mcp_error)
        .unwrap_err();
    assert_eq!(unknown.code.0, -32602, "unknown tool is invalid params");
    client
        .read_resource(ReadResourceRequestParams::new("nugget://entity/ent_sam"))
        .await
        .unwrap();
    let missing = client
        .read_resource(ReadResourceRequestParams::new("file:///etc/passwd"))
        .await
        .map(|_| ())
        .map_err(mcp_error)
        .unwrap_err();
    assert_eq!(missing.code.0, -32002, "resource not found");
    client.cancel().await.unwrap();

    let entries = log.entries();
    let summary: Vec<(String, String, String)> = entries
        .iter()
        .map(|e| {
            (
                e.action.clone(),
                e.target.clone(),
                e.detail["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let want = [
        ("mcp.tool_call", "search", "ok"),
        ("mcp.tool_call", "search", "rejected"),
        ("mcp.tool_call", "rm_rf", "rejected"),
        ("mcp.resource_read", "nugget://entity/ent_sam", "ok"),
        ("mcp.resource_read", "file:///etc/passwd", "rejected"),
    ]
    .map(|(a, t, o)| (a.to_owned(), t.to_owned(), o.to_owned()));
    assert_eq!(summary, want);
}

#[tokio::test]
async fn resources_list_refuses_cursors_it_never_handed_out() {
    let server = McpServer::builder(backend(false)).build();
    let client = stdio_client(&server).await;
    for bad in [
        "not-a-cursor",
        "",
        "-1",
        "+5",
        " 0",
        "99999999999999999999999999",
    ] {
        let err = client
            .list_resources(Some(
                PaginatedRequestParams::default().with_cursor(Some(bad.into())),
            ))
            .await
            .map(|_| ())
            .map_err(mcp_error)
            .unwrap_err();
        assert_eq!(err.code.0, -32602, "cursor {bad:?}");
    }
    let first = client.list_resources(None).await.unwrap();
    assert!(!first.resources.is_empty());
    let from_zero = client
        .list_resources(Some(
            PaginatedRequestParams::default().with_cursor(Some("0".into())),
        ))
        .await
        .unwrap();
    assert_eq!(first.resources.len(), from_zero.resources.len());
    // Tools are a single page; a cursor is ignored rather than refused.
    let tools = client
        .list_tools(Some(
            PaginatedRequestParams::default().with_cursor(Some("whatever".into())),
        ))
        .await
        .unwrap();
    assert_eq!(tools.tools.len(), nugget_mcp::tools::ALL_TOOLS.len());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn cancelled_requests_stop_and_are_audited_once() {
    let log = AuditLog::default();
    let server = McpServer::builder(backend(false)).audit(log.sink()).build();
    let c = client(all_scopes());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let f = server
        .call_tool_until(
            &c,
            Transport::Stdio,
            "search",
            &obj(json!({"query": "apollo"})),
            &cancel,
        )
        .await
        .unwrap_err();
    assert_eq!(f.outcome, nugget_mcp::audit::Outcome::Cancelled);
    let f = server
        .read_resource_until(&c, Transport::Stdio, "nugget://entity/ent_sam", &cancel)
        .await
        .unwrap_err();
    assert_eq!(f.outcome, nugget_mcp::audit::Outcome::Cancelled);
    let outcomes: Vec<_> = log
        .entries()
        .iter()
        .map(|e| (e.action.clone(), e.detail["outcome"].clone()))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("mcp.tool_call".to_owned(), json!("cancelled")),
            ("mcp.resource_read".to_owned(), json!("cancelled")),
        ]
    );
    // Not cancelled: runs normally.
    server
        .call_tool_until(
            &c,
            Transport::Stdio,
            "search",
            &obj(json!({"query": "apollo"})),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
}

const LIST: &str = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#;

async fn post(url: &str, token: Option<&str>) -> reqwest::Response {
    let mut req = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(LIST);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    req.send().await.unwrap()
}

async fn json_error(r: reqwest::Response) -> Value {
    assert_eq!(r.headers()["content-type"], "application/json");
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["id"], Value::Null);
    v["error"].clone()
}

#[tokio::test]
async fn http_refusals_are_json_rpc_errors_sdk_clients_can_show() {
    let (reg, _) = registry();
    let server = McpServer::builder(backend(false))
        .registry(reg.clone())
        .config(McpConfig {
            rate_limit: RateLimit {
                burst: 1,
                per_minute: 1,
            },
        })
        .build();
    let handle = server
        .serve_http(HttpConfig {
            port: 0,
            ..HttpConfig::default()
        })
        .await
        .unwrap();
    let url = handle.url();
    let (_, token) = reg.create("A", all_scopes()).unwrap();

    let r = post(&url, Some("ngt_wrong")).await;
    assert_eq!(r.status(), 401);
    assert!(r.headers().contains_key("www-authenticate"));
    let e = json_error(r).await;
    assert_eq!(e["code"], -32600);
    assert!(
        e["message"].as_str().unwrap().contains("bearer token"),
        "{e}"
    );
    assert_eq!(e["data"]["http_status"], 401);

    assert_eq!(post(&url, Some(token.expose())).await.status(), 200);
    let r = post(&url, Some(token.expose())).await;
    assert_eq!(r.status(), 429);
    assert!(r.headers().contains_key("retry-after"));
    let e = json_error(r).await;
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .starts_with("rate limited; retry after"),
        "{e}"
    );

    // A refused request's body is read before answering: a client still sending its body when
    // the refusal is ready must get the refusal, not a connection reset (closing a socket with
    // unread input resets it, and the client loses the response).
    for _ in 0..5 {
        let mut s = tokio::net::TcpStream::connect(handle.local_addr())
            .await
            .unwrap();
        let body = LIST.repeat(40);
        let head = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Authorization: Bearer ngt_wrong\r\nContent-Length: {}\r\n\r\n",
            handle.local_addr(),
            body.len()
        );
        s.write_all(head.as_bytes()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        s.write_all(body.as_bytes()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut buf = vec![0u8; 4096];
        let n = s
            .read(&mut buf)
            .await
            .expect("the refusal arrives, not a reset");
        let text = String::from_utf8_lossy(&buf[..n]);
        assert!(text.starts_with("HTTP/1.1 401"), "{text}");
    }
    handle.shutdown().await;
}

fn outcomes(log: &AuditLog) -> Vec<(String, String, String)> {
    log.entries()
        .iter()
        .map(|e| {
            (
                e.action.clone(),
                e.target.clone(),
                e.detail["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Wait (bounded) until the log holds `n` entries.
async fn wait_for_entries(log: &AuditLog, n: usize) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while log.entries().len() < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {n} audit entries, have {:?}",
            outcomes(log)
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn refused_stdio_calls_are_audited_coalesced() {
    let log = AuditLog::default();
    let (reg, _) = registry();
    let (info, _token) = reg.create("Runaway", all_scopes()).unwrap();
    let server = McpServer::builder(backend(false))
        .registry(reg.clone())
        .audit(log.sink())
        .config(McpConfig {
            rate_limit: RateLimit {
                burst: 2,
                per_minute: 1,
            },
        })
        .build();
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let (r, w) = tokio::io::split(server_io);
    let s = server.clone();
    tokio::spawn(async move { s.serve_io(info, Transport::Stdio, r, w).await });
    let c: Client = ().serve(client_io).await.unwrap();
    let search =
        || CallToolRequestParams::new("search").with_arguments(obj(json!({"query": "apollo"})));
    for _ in 0..2 {
        c.call_tool(search()).await.unwrap();
    }
    // A burst over the limit: refused, one coalesced entry for all of it.
    for _ in 0..10 {
        let e = mcp_error(c.call_tool(search()).await.unwrap_err());
        assert!(e.message.contains("rate limited"), "{e:?}");
    }
    let read = c
        .read_resource(ReadResourceRequestParams::new("nugget://entity/ent_sam"))
        .await;
    assert!(read.is_err(), "reads share the limit");
    let entries = log.entries();
    assert_eq!(
        outcomes(&log),
        [
            ("mcp.tool_call", "search", "ok"),
            ("mcp.tool_call", "search", "ok"),
            ("mcp.tool_call", "search", "rate_limited"),
        ]
        .map(|(a, t, o)| (a.to_owned(), t.to_owned(), o.to_owned()))
    );
    assert_eq!(entries[2].detail["refused_since_last_entry"], 0);
    assert_eq!(entries[2].detail["transport"], "stdio");

    // Revoked: the next call is refused and audited once as denied, then coalesced.
    let id = reg.list().unwrap()[0].id.clone();
    assert!(reg.revoke(&id).unwrap());
    for _ in 0..3 {
        let e = mcp_error(c.call_tool(search()).await.unwrap_err());
        assert!(e.message.contains("revoked"), "{e:?}");
    }
    let last = outcomes(&log).pop().unwrap();
    assert_eq!(log.entries().len(), 4);
    assert_eq!(
        last,
        ("mcp.tool_call".into(), "search".into(), "denied".into())
    );
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn http_rate_limit_refusals_are_audited_coalesced() {
    let log = AuditLog::default();
    let (reg, _) = registry();
    let server = McpServer::builder(backend(false))
        .registry(reg.clone())
        .audit(log.sink())
        .config(McpConfig {
            rate_limit: RateLimit {
                burst: 1,
                per_minute: 1,
            },
        })
        .build();
    let handle = server
        .serve_http(HttpConfig {
            port: 0,
            ..HttpConfig::default()
        })
        .await
        .unwrap();
    let url = handle.url();
    let (_, token) = reg.create("A", all_scopes()).unwrap();
    assert_eq!(post(&url, Some(token.expose())).await.status(), 200);
    for _ in 0..5 {
        assert_eq!(post(&url, Some(token.expose())).await.status(), 429);
    }
    // Unattributable: a wrong token names no client.
    assert_eq!(post(&url, Some("ngt_wrong")).await.status(), 401);
    let entries = log.entries();
    assert_eq!(
        outcomes(&log),
        [(
            "mcp.request_refused".into(),
            "http".into(),
            "rate_limited".into()
        )]
    );
    assert_eq!(entries[0].detail["transport"], "loopback_http");
    handle.shutdown().await;
}

#[tokio::test]
async fn a_refused_request_expecting_100_continue_is_not_drained() {
    let (reg, _) = registry();
    let server = McpServer::builder(backend(false))
        .registry(reg.clone())
        .build();
    let handle = server
        .serve_http(HttpConfig {
            port: 0,
            ..HttpConfig::default()
        })
        .await
        .unwrap();
    let mut s = tokio::net::TcpStream::connect(handle.local_addr())
        .await
        .unwrap();
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
         Authorization: Bearer ngt_wrong\r\nExpect: 100-continue\r\nContent-Length: 500000\r\n\r\n",
        handle.local_addr()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), s.read(&mut buf))
        .await
        .expect("answered without waiting for the body")
        .unwrap();
    let text = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
    assert!(text.starts_with("http/1.1 401"), "{text}");
    assert!(!text.contains("100 continue"), "no upload invited: {text}");
    assert!(text.contains("connection: close"), "{text}");
    handle.shutdown().await;
}

#[tokio::test]
async fn a_cancelled_call_in_flight_stops_and_is_audited_once() {
    let log = AuditLog::default();
    let b = backend(false);
    b.set_delay(std::time::Duration::from_secs(30));
    let server = McpServer::builder(b).audit(log.sink()).build();
    let client = stdio_client(&server).await;
    // Dropping the call sends notifications/cancelled for it (rmcp's cancel-on-drop).
    let call = client.call_tool(
        CallToolRequestParams::new("get_context").with_arguments(obj(json!({"query": "apollo"}))),
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), call)
            .await
            .is_err(),
        "the slow call is still running"
    );
    wait_for_entries(&log, 1).await;
    assert_eq!(
        outcomes(&log),
        [(
            "mcp.tool_call".into(),
            "get_context".into(),
            "cancelled".into()
        )]
    );
    // The session is healthy and nothing else was recorded.
    client.list_all_tools().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(log.entries().len(), 1);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn cancel_racing_completion_is_audited_exactly_once() {
    let log = AuditLog::default();
    let b = backend(false);
    b.set_delay(std::time::Duration::from_millis(20));
    let server = McpServer::builder(b).audit(log.sink()).build();
    let client = stdio_client(&server).await;
    const CALLS: usize = 40;
    for i in 0..CALLS {
        // Cancel anywhere from before to well after the 20 ms the call takes (never so early
        // that the request might not have been sent yet).
        let wait = std::time::Duration::from_millis(10 + (i as u64 * 7) % 41);
        let call = client.call_tool(
            CallToolRequestParams::new("search").with_arguments(obj(json!({"query": "apollo"}))),
        );
        let _ = tokio::time::timeout(wait, call).await;
    }
    wait_for_entries(&log, CALLS).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let all = outcomes(&log);
    assert_eq!(all.len(), CALLS, "one entry per call: {all:?}");
    assert!(
        all.iter().all(|(_, _, o)| o == "ok" || o == "cancelled"),
        "{all:?}"
    );
    assert!(all.iter().any(|(_, _, o)| o == "cancelled"), "{all:?}");
    client.cancel().await.unwrap();
}
