//! Fixes found by running the official TypeScript and Python SDK clients against Nugget
//! (`crates/nugget-mcp/compat`): every tool call and resource read is audited exactly once
//! (unknown tools and cancelled requests too), `resources/list` refuses cursors it never handed
//! out, cancelled requests stop, and HTTP refusals carry a JSON-RPC error body that SDK clients
//! can show.

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
