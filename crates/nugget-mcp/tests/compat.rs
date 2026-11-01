//! Fixes found by running the official TypeScript and Python SDK clients against Nugget
//! (`crates/nugget-mcp/compat`): every tool call and resource read that runs is audited exactly
//! once (unknown tools and cancelled requests too, including a cancellation racing completion),
//! `resources/list` refuses cursors it never handed out, cancelled requests stop, and HTTP
//! refusals carry a JSON-RPC error body that SDK clients can show.
//!
//! Refused calls (rate limited, revoked) are audited coalesced: at most one entry per client and
//! outcome per `REFUSAL_AUDIT_WINDOW`. Over HTTP a rate-limited request is refused before its
//! body is read (`mcp.request_refused`, target `http`); a request with a bad token names no
//! client and is not audited.

mod common;

use std::time::Duration;

use common::*;
use nugget_mcp::audit::Outcome;
use nugget_mcp::{HttpConfig, McpConfig, McpServer, RateLimit, Transport};
use rmcp::model::{CallToolRequestParams, PaginatedRequestParams, ReadResourceRequestParams};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

fn search() -> CallToolRequestParams {
    tool_call("search", json!({"query": "apollo"}))
}

fn limited(burst: u32) -> McpConfig {
    McpConfig {
        rate_limit: RateLimit {
            burst,
            per_minute: 1,
        },
    }
}

#[tokio::test]
async fn every_call_and_read_is_audited_once_including_unknown_tools() {
    let log = AuditLog::default();
    let server = McpServer::builder(backend(false)).audit(log.sink()).build();
    let client = stdio_client(&server, common::client(all_scopes())).await;

    let ok = client.call_tool(search()).await.unwrap();
    assert_ne!(ok.is_error, Some(true));
    let bad = client
        .call_tool(tool_call("search", json!({"limit": 3})))
        .await
        .unwrap();
    assert_eq!(
        bad.is_error,
        Some(true),
        "invalid arguments are a tool error"
    );
    let unknown = mcp_error(
        client
            .call_tool(CallToolRequestParams::new("rm_rf"))
            .await
            .unwrap_err(),
    );
    assert_eq!(unknown.code.0, -32602, "unknown tool is invalid params");
    client
        .read_resource(ReadResourceRequestParams::new("nugget://entity/ent_sam"))
        .await
        .unwrap();
    let missing = mcp_error(
        client
            .read_resource(ReadResourceRequestParams::new("file:///etc/passwd"))
            .await
            .unwrap_err(),
    );
    assert_eq!(missing.code.0, -32002, "resource not found");
    client.cancel().await.unwrap();

    assert_eq!(
        outcomes(&log),
        triples(&[
            ("mcp.tool_call", "search", "ok"),
            ("mcp.tool_call", "search", "rejected"),
            ("mcp.tool_call", "rm_rf", "rejected"),
            ("mcp.resource_read", "nugget://entity/ent_sam", "ok"),
            ("mcp.resource_read", "file:///etc/passwd", "rejected"),
        ])
    );
}

#[tokio::test]
async fn resources_list_refuses_cursors_it_never_handed_out() {
    let server = McpServer::builder(backend(false)).build();
    let client = stdio_client(&server, common::client(all_scopes())).await;
    let page = |c: &str| PaginatedRequestParams::default().with_cursor(Some(c.into()));
    for bad in [
        "not-a-cursor",
        "",
        "-1",
        "+5",
        " 0",
        "99999999999999999999999999",
    ] {
        let err = mcp_error(client.list_resources(Some(page(bad))).await.unwrap_err());
        assert_eq!(err.code.0, -32602, "cursor {bad:?}");
    }
    let first = client.list_resources(None).await.unwrap();
    assert!(!first.resources.is_empty());
    let from_zero = client.list_resources(Some(page("0"))).await.unwrap();
    assert_eq!(first.resources.len(), from_zero.resources.len());
    // Tools are a single page; a cursor is ignored rather than refused.
    let tools = client.list_tools(Some(page("whatever"))).await.unwrap();
    assert_eq!(tools.tools.len(), nugget_mcp::tools::ALL_TOOLS.len());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn cancelled_requests_stop_and_are_audited_once() {
    let log = AuditLog::default();
    let server = McpServer::builder(backend(false)).audit(log.sink()).build();
    let c = common::client(all_scopes());
    let args = obj(json!({"query": "apollo"}));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let f = server
        .call_tool_until(&c, Transport::Stdio, "search", &args, &cancel)
        .await
        .unwrap_err();
    assert_eq!(f.outcome, Outcome::Cancelled);
    let f = server
        .read_resource_until(&c, Transport::Stdio, "nugget://entity/ent_sam", &cancel)
        .await
        .unwrap_err();
    assert_eq!(f.outcome, Outcome::Cancelled);
    assert_eq!(
        outcomes(&log),
        triples(&[
            ("mcp.tool_call", "search", "cancelled"),
            ("mcp.resource_read", "nugget://entity/ent_sam", "cancelled"),
        ])
    );
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

/// Send `head` then `body` on a raw connection and return the first bytes of the answer.
async fn raw_exchange(addr: std::net::SocketAddr, head: String, body: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(head.as_bytes()).await.unwrap();
    if !body.is_empty() {
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.write_all(body.as_bytes()).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
        .await
        .expect("answered")
        .expect("the refusal arrives, not a reset");
    String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase()
}

#[tokio::test]
async fn http_refusals_are_json_rpc_errors_and_rate_limits_are_audited_per_client() {
    let log = AuditLog::default();
    let (reg, _) = registry();
    let server = McpServer::builder(backend(false))
        .registry(reg.clone())
        .audit(log.sink())
        .config(limited(1))
        .build();
    let handle = server
        .serve_http(HttpConfig {
            port: 0,
            ..HttpConfig::default()
        })
        .await
        .unwrap();
    let (url, addr) = (handle.url(), handle.local_addr());
    let (_, token) = reg.create("A", all_scopes()).unwrap();
    let (_, other) = reg.create("B", all_scopes()).unwrap();

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
    for _ in 0..5 {
        let r = post(&url, Some(token.expose())).await;
        assert_eq!(r.status(), 429);
        assert!(
            r.headers()["retry-after"]
                .to_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                >= 1
        );
        let e = json_error(r).await;
        let msg = e["message"].as_str().unwrap();
        assert!(msg.starts_with("rate limited; retry after"), "{e}");
    }
    assert_eq!(
        post(&url, Some(other.expose())).await.status(),
        200,
        "other clients unaffected"
    );
    // One coalesced entry for the burst; the wrong token is unattributable and not audited.
    assert_eq!(
        outcomes(&log),
        triples(&[("mcp.request_refused", "http", "rate_limited")])
    );
    assert_eq!(log.entries()[0].detail["transport"], "loopback_http");

    // A refused request's body is read before answering: a client still sending its body when
    // the refusal is ready must get the refusal, not a connection reset.
    for _ in 0..5 {
        let body = LIST.repeat(40);
        let head = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
             Authorization: Bearer ngt_wrong\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let text = raw_exchange(addr, head, &body).await;
        assert!(text.starts_with("http/1.1 401"), "{text}");
    }
    // ...unless it waits for `100 Continue`: then it is answered at once and closed.
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Authorization: Bearer ngt_wrong\r\nExpect: 100-continue\r\nContent-Length: 500000\r\n\r\n"
    );
    let text = raw_exchange(addr, head, "").await;
    assert!(text.starts_with("http/1.1 401"), "{text}");
    assert!(!text.contains("100 continue"), "no upload invited: {text}");
    assert!(text.contains("connection: close"), "{text}");
    handle.shutdown().await;
}

#[tokio::test]
async fn refused_stdio_calls_are_audited_coalesced() {
    let log = AuditLog::default();
    let (reg, _) = registry();
    let (info, _token) = reg.create("Runaway", all_scopes()).unwrap();
    let server = McpServer::builder(backend(false))
        .registry(reg.clone())
        .audit(log.sink())
        .config(limited(2))
        .build();
    let c = stdio_client(&server, info).await;
    for _ in 0..2 {
        c.call_tool(search()).await.unwrap();
    }
    assert!(
        reg.list().unwrap()[0].last_used.is_some(),
        "last_used tracked"
    );
    // A burst over the limit: refused, one coalesced entry for all of it.
    for _ in 0..10 {
        let e = mcp_error(c.call_tool(search()).await.unwrap_err());
        assert!(e.message.contains("rate limited"), "{e:?}");
    }
    let read = c
        .read_resource(ReadResourceRequestParams::new("nugget://entity/ent_sam"))
        .await;
    assert!(read.is_err(), "reads share the limit");
    assert_eq!(
        outcomes(&log),
        triples(&[
            ("mcp.tool_call", "search", "ok"),
            ("mcp.tool_call", "search", "ok"),
            ("mcp.tool_call", "search", "rate_limited"),
        ])
    );
    let entries = log.entries();
    assert_eq!(entries[2].detail["refused_since_last_entry"], 0);
    assert_eq!(entries[2].detail["transport"], "stdio");

    // Revoked: the next call is refused and audited once as denied, then coalesced.
    let id = reg.list().unwrap()[0].id.clone();
    assert!(reg.revoke(&id).unwrap());
    for _ in 0..3 {
        let e = mcp_error(c.call_tool(search()).await.unwrap_err());
        assert!(e.message.contains("revoked"), "{e:?}");
    }
    assert_eq!(log.entries().len(), 4);
    assert_eq!(
        outcomes(&log).pop().unwrap(),
        triples(&[("mcp.tool_call", "search", "denied")])[0]
    );
    c.cancel().await.unwrap();
}

/// Wait (bounded) until the log holds `n` entries.
async fn wait_for_entries(log: &AuditLog, n: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while log.entries().len() < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {n} audit entries, have {:?}",
            outcomes(log)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_cancelled_call_in_flight_stops_and_is_audited_once() {
    let log = AuditLog::default();
    let b = backend(false);
    b.set_delay(Duration::from_secs(30));
    let server = McpServer::builder(b).audit(log.sink()).build();
    let client = stdio_client(&server, common::client(all_scopes())).await;
    // Dropping the call sends notifications/cancelled for it (rmcp's cancel-on-drop).
    let call = client.call_tool(tool_call("get_context", json!({"query": "apollo"})));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), call)
            .await
            .is_err(),
        "the slow call is still running"
    );
    wait_for_entries(&log, 1).await;
    assert_eq!(
        outcomes(&log),
        triples(&[("mcp.tool_call", "get_context", "cancelled")])
    );
    // The session is healthy and nothing else was recorded.
    client.list_all_tools().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(log.entries().len(), 1);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn cancel_racing_completion_is_audited_exactly_once() {
    let log = AuditLog::default();
    let b = backend(false);
    b.set_delay(Duration::from_millis(20));
    let server = McpServer::builder(b).audit(log.sink()).build();
    let client = stdio_client(&server, common::client(all_scopes())).await;
    const CALLS: usize = 40;
    for i in 0..CALLS {
        // Cancel anywhere from before to well after the 20 ms the call takes (never so early
        // that the request might not have been sent yet).
        let wait = Duration::from_millis(10 + (i as u64 * 7) % 41);
        let _ = tokio::time::timeout(wait, client.call_tool(search())).await;
    }
    wait_for_entries(&log, CALLS).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let all = outcomes(&log);
    assert_eq!(all.len(), CALLS, "one entry per call: {all:?}");
    assert!(
        all.iter().all(|(_, _, o)| o == "ok" || o == "cancelled"),
        "{all:?}"
    );
    assert!(all.iter().any(|(_, _, o)| o == "cancelled"), "{all:?}");
    client.cancel().await.unwrap();
}
