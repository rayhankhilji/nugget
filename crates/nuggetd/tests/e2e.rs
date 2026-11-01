//! End-to-end: the daemon in-process on a temporary home, the real IPC client, model traffic
//! from a wiremock OpenAI-compatible server (plus a raw TCP server for a slow, never-ending
//! stream).

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use common::*;
use futures::{Stream, StreamExt};
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::egress::EgressEvent;
use nugget_core::ipc::{
    ChatDelta, ChatDone, ChatStarted, ModelInfo, ProviderInfo, SearchHit, StatusResponse, method,
    notification,
};
use nugget_infer::probe::vision::read_code_png;
use nugget_ipc::{Client, Notification, code};
use nugget_store::{NewChunk, NewItem, content_hash};
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::method as http_method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn text_chunks(parts: &[&str]) -> Vec<Value> {
    let mut v: Vec<Value> = parts
        .iter()
        .map(|p| json!({"choices": [{"index": 0, "delta": {"content": p}}]}))
        .collect();
    v.push(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}));
    v.push(json!({"choices": [], "usage": {"prompt_tokens": 9, "completion_tokens": 2}}));
    v
}

/// Answers chat like a capable model (including the probe's vision, tool and JSON checks) and
/// embeds anything.
struct SmartModel;

impl Respond for SmartModel {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        if req.url.path().ends_with("/embeddings") {
            let n = body["input"].as_array().unwrap().len();
            let data: Vec<Value> = (0..n)
                .map(|i| json!({"index": i, "embedding": [0.1 * (i as f32 + 1.0), 0.2, 0.3]}))
                .collect();
            return ResponseTemplate::new(200).set_body_json(json!({"data": data}));
        }
        let content = &body["messages"].as_array().unwrap().last().unwrap()["content"];
        let chunks = if let Some(parts) = content.as_array()
            && let Some(img) = parts.iter().find(|p| p["type"] == "image_url")
        {
            let url = img["image_url"]["url"].as_str().unwrap();
            let png = B64.decode(url.split_once(";base64,").unwrap().1).unwrap();
            text_chunks(&[&read_code_png(&png, 4).unwrap()])
        } else if body.get("tools").is_some() {
            let text = content.as_str().unwrap();
            let start = text.find('"').unwrap() + 1;
            let value = &text[start..start + text[start..].find('"').unwrap()];
            let name = body["tools"][0]["function"]["name"].as_str().unwrap();
            vec![
                json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": name, "arguments": json!({"value": value}).to_string()}}]}}]}),
                json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
            ]
        } else if body.get("response_format").is_some() {
            text_chunks(&[r#"{"answer": 7, "#, r#""unit": "apples"}"#])
        } else if content.as_str() == Some("say hello") {
            text_chunks(&["Hel", "lo"])
        } else {
            text_chunks(&["po", "ng"])
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(sse(&chunks))
    }
}

// ---- a slow, never-ending SSE server ----

struct SlowServer {
    addr: SocketAddr,
    /// Set once a write fails, i.e. the client dropped the HTTP stream.
    closed: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
}

async fn slow_server() -> SlowServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let closed = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(AtomicUsize::new(0));
    let (c, r) = (closed.clone(), requests.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (closed, requests) = (c.clone(), r.clone());
            tokio::spawn(async move {
                // Read the request head and body.
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let head_end = loop {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map(|v| v.trim().parse().unwrap())
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                requests.fetch_add(1, Ordering::SeqCst);
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\ntransfer-encoding: chunked\r\n\r\n";
                if sock.write_all(head.as_bytes()).await.is_err() {
                    closed.store(true, Ordering::SeqCst);
                    return;
                }
                for i in 0..600 {
                    let data = format!(
                        "data: {}\n\n",
                        json!({"choices": [{"index": 0, "delta": {"content": format!("tick{i} ")}}]})
                    );
                    let chunk = format!("{:x}\r\n{data}\r\n", data.len());
                    if sock.write_all(chunk.as_bytes()).await.is_err()
                        || sock.flush().await.is_err()
                    {
                        closed.store(true, Ordering::SeqCst);
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            });
        }
    });
    SlowServer {
        addr,
        closed,
        requests,
    }
}

struct Rig {
    h: Harness,
    mock: MockServer,
    slow: SlowServer,
}

/// `dead` (nothing listens) ahead of `mock` in the light tier, `slow` for agentic, and
/// `needs-key`, which cannot be built (its key variable is unset).
async fn start() -> Rig {
    let mock = MockServer::start().await;
    Mock::given(http_method("POST"))
        .respond_with(SmartModel)
        .mount(&mock)
        .await;
    let slow = slow_server().await;
    let config = format!(
        r#"{NO_RUNTIME}
[[providers]]
id = "dead"
kind = "openai_compatible"
base_url = "http://127.0.0.1:9/v1"
model = "dead-model"
tiers = ["light"]
max_retries = 0
connect_timeout_secs = 2

[[providers]]
id = "mock"
kind = "openai_compatible"
base_url = "{}/v1"
model = "test-model"
tiers = ["light", "synthesis"]
max_retries = 0
context_tokens = 4096

[[providers]]
id = "slow"
kind = "openai_compatible"
base_url = "http://{}/v1"
model = "slow-model"
tiers = ["agentic"]
max_retries = 0

[[providers]]
id = "needs-key"
kind = "anthropic"
model = "m"
api_key = "env:NUGGETD_TEST_DEFINITELY_UNSET_KEY"
tiers = ["synthesis"]
"#,
        mock.uri(),
        slow.addr
    );
    let h = Harness::start(&config).await;
    Rig { h, mock, slow }
}

fn chat_request(provider: Option<&str>, tier: &str, text: &str) -> Value {
    json!({"provider": provider, "tier": tier, "messages": [
        {"role": "system", "text": "You are terse."}, {"role": "user", "text": text}]})
}

/// Collect one stream's deltas until its `chat.done`.
async fn collect_chat(
    notes: &mut (impl Stream<Item = Notification> + Unpin),
    stream_id: &str,
) -> (String, ChatDone) {
    let mut text = String::new();
    loop {
        let n = tokio::time::timeout(CALL, notes.next())
            .await
            .expect("chat notification in time")
            .expect("connection open");
        match n.method.as_str() {
            notification::CHAT_DELTA => {
                let d: ChatDelta = n.params_as().unwrap();
                if d.stream_id == stream_id {
                    text.push_str(&d.text);
                }
            }
            notification::CHAT_DONE => {
                let d: ChatDone = n.params_as().unwrap();
                if d.stream_id == stream_id {
                    return (text, d);
                }
            }
            _ => {}
        }
    }
}

async fn wait_closed(slow: &SlowServer) {
    eventually("provider connection closed", || async {
        slow.closed.load(Ordering::SeqCst)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_chat_search_egress_and_models() {
    let Rig {
        mut h,
        mock,
        slow: _slow,
    } = start().await;
    let item = NewItem::new("files:test", "notes/fox.md", content_hash("fox"))
        .with_title("Fox notes")
        .with_text("The quick brown fox jumps over the lazy dog.");
    h.store.upsert_item(&item).unwrap();
    h.store
        .replace_chunks(
            &item.item,
            &[
                NewChunk::text("The quick brown fox jumps over the lazy dog."),
                NewChunk::text("Foxes are small omnivorous mammals."),
            ],
        )
        .unwrap();

    let status: StatusResponse = h.call(method::STATUS, Value::Null).await;
    assert_eq!(status.protocol, nugget_core::ipc::PROTOCOL_VERSION);
    assert_eq!((status.items, status.chunks), (1, 2));
    let ids: Vec<&str> = status.providers.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["dead", "mock", "slow", "needs-key"],
        "config order"
    );
    assert_eq!(status.providers[1].locality, Locality::OnDevice);
    assert_eq!(
        status.providers[1].tiers,
        vec![Tier::Light, Tier::Synthesis]
    );
    // The unbuildable provider is listed with why, instead of vanishing.
    let broken = &status.providers[3];
    assert_eq!(broken.locality, Locality::Cloud);
    let reason = broken.error.as_deref().expect("build error reported");
    assert!(
        reason.contains("NUGGETD_TEST_DEFINITELY_UNSET_KEY"),
        "{reason}"
    );
    eventually("audit chain checked at startup", || async {
        let s: StatusResponse = h.call(method::STATUS, json!({})).await;
        s.audit_chain_ok == Some(true)
    })
    .await;

    // Chat routed by tier: deltas, then done with usage; the provider saw both turns.
    let mut notes = h.client.notifications();
    let started: ChatStarted = h
        .call(method::CHAT, chat_request(None, "synthesis", "say hello"))
        .await;
    assert_eq!(started.provider.as_str(), "mock");
    let (text, done) = collect_chat(&mut notes, &started.stream_id).await;
    assert_eq!(text, "Hello");
    assert_eq!(
        (done.error, done.input_tokens, done.output_tokens),
        (None, Some(9), Some(2))
    );
    let reqs = mock.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&reqs.last().unwrap().body).unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["messages"][0]["content"], "You are terse.");
    assert_eq!(body["messages"][1]["content"], "say hello");

    // The first chat after an outage falls back to the next provider, and `ChatStarted` and
    // the audit name the provider that serves it.
    let started: ChatStarted = h
        .call(method::CHAT, chat_request(None, "light", "ping"))
        .await;
    assert_eq!(started.provider.as_str(), "mock");
    let (text, done) = collect_chat(&mut notes, &started.stream_id).await;
    assert_eq!((text.as_str(), done.error), ("pong", None));

    // An explicit provider is never substituted; routing and parameter errors are RPC errors.
    for (params, want) in [
        (chat_request(Some("dead"), "light", "x"), code::PROVIDER),
        (chat_request(None, "embedding", "x"), code::NOT_FOUND),
        (chat_request(Some("nope"), "light", "x"), code::NOT_FOUND),
        (
            chat_request(Some("needs-key"), "light", "x"),
            code::NOT_FOUND,
        ),
        (
            json!({"provider": null, "tier": "light", "messages": []}),
            code::INVALID_PARAMS,
        ),
        (json!({"tier": "bogus"}), code::INVALID_PARAMS),
    ] {
        assert_eq!(h.call_err(method::CHAT, params).await.0, want);
    }
    assert_eq!(
        h.call_err("no.such.method", json!({})).await.0,
        code::METHOD_NOT_FOUND
    );

    // Search: one hit per item (its best chunk).
    let hits: Vec<SearchHit> = h
        .call(method::SEARCH, json!({"query": "fox", "limit": 10}))
        .await;
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].item, item.item);
    assert!(hits[0].chunk_id.is_some() && hits[0].score > 0.0);

    // The loopback provider never shows up in the egress ledger.
    let egress: Vec<EgressEvent> = h.call(method::EGRESS_RECENT, json!({"limit": 50})).await;
    assert!(egress.is_empty(), "{egress:?}");

    // models.list: the bundled manifest, nothing installed.
    let manifest = nugget_models::default_manifest().unwrap();
    let models: Vec<ModelInfo> = h.call(method::MODELS_LIST, json!({})).await;
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    let want: Vec<&str> = manifest.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, want);
    assert!(models.iter().all(|m| !m.installed && !m.update_available));
    let (c, _) = h
        .call_err(method::MODELS_INSTALL, json!({"id": "no-such-model"}))
        .await;
    assert_eq!(c, code::NOT_FOUND);

    // Chats are audited (by serving provider), without message content.
    let chats: Vec<_> = h.audit().into_iter().filter(|a| a.0 == "chat").collect();
    assert_eq!(chats.len(), 2);
    assert!(chats.iter().all(|c| c.1 == "mock"));
    assert!(!chats[0].2.to_string().contains("say hello"));
    h.store.verify_audit_chain().unwrap();

    // A second daemon on the same home reports the first one.
    let mut opts = DaemonOptions::new(h.paths.clone());
    opts.store_key = Some(nugget_store::StoreKey::from_bytes(KEY));
    match Daemon::start(opts).await.unwrap() {
        Startup::AlreadyRunning { socket } => {
            assert_eq!(socket, nugget_ipc::socket_display(&h.paths));
        }
        Startup::Ready(_) => panic!("second daemon must not start"),
    }
    h.stop().await;
    assert!(Client::connect(&h.paths).await.is_err(), "socket closed");
}

/// Cancel, a client that goes away and shutdown all stop a slow stream and its HTTP request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_streams_stop_on_cancel_disconnect_and_shutdown() {
    let Rig {
        mut h,
        slow,
        mock: _mock,
    } = start().await;
    let mut notes = h.client.notifications();
    let started: ChatStarted = h
        .call(method::CHAT, chat_request(None, "agentic", "go slowly"))
        .await;
    assert_eq!(started.provider.as_str(), "slow");
    let first = tokio::time::timeout(CALL, notes.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.method, notification::CHAT_DELTA);
    let () = h
        .call(method::CHAT_CANCEL, json!({"stream_id": started.stream_id}))
        .await;
    let (_, done) = collect_chat(&mut notes, &started.stream_id).await;
    assert_eq!(done.error.as_deref(), Some("cancelled"));
    let quiet = tokio::time::timeout(Duration::from_millis(600), async {
        while let Some(n) = notes.next().await {
            if n.method == notification::CHAT_DELTA {
                return Some(n);
            }
        }
        None
    })
    .await;
    assert!(quiet.is_err(), "unexpected delta after cancel: {quiet:?}");
    wait_closed(&slow).await;
    assert_eq!(slow.requests.load(Ordering::SeqCst), 1);
    // Cancelling an unknown stream is a no-op.
    let () = h
        .call(method::CHAT_CANCEL, json!({"stream_id": "unknown"}))
        .await;

    // A slow stream whose client goes away is stopped too.
    let client2 = Client::connect(&h.paths).await.unwrap();
    let mut notes2 = client2.notifications();
    let _: ChatStarted = client2
        .call(method::CHAT, &chat_request(Some("slow"), "light", "again"))
        .await
        .unwrap();
    let first = tokio::time::timeout(CALL, notes2.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.method, notification::CHAT_DELTA);
    slow.closed.store(false, Ordering::SeqCst);
    drop((notes2, client2));
    wait_closed(&slow).await;

    // Shutdown ends open chats with `chat.done`.
    let started: ChatStarted = h
        .call(method::CHAT, chat_request(None, "agentic", "go slowly"))
        .await;
    let _ = h.stop.take().unwrap().send(());
    let (_, done) = collect_chat(&mut notes, &started.stream_id).await;
    assert_eq!(done.error.as_deref(), Some("nuggetd is shutting down"));
    tokio::time::timeout(CALL, h.server.take().unwrap())
        .await
        .expect("daemon stops promptly")
        .unwrap()
        .unwrap();
}

/// A probe is audited and persisted across restarts and routes capability requests; probing an
/// unreachable provider records nothing, so it still counts as unprobed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probes_are_persisted_and_failed_probes_record_nothing() {
    let Rig {
        h,
        mock: _mock,
        slow: _slow,
    } = start().await;
    let (c, msg) = h
        .call_err(method::PROVIDERS_PROBE, json!({"provider": "dead"}))
        .await;
    assert_eq!(c, code::PROVIDER);
    assert!(msg.contains("unreachable"), "{msg}");
    assert!(!nuggetd::probes::ProbeCache::path_for(&h.paths).exists());

    let caps: Capabilities = h
        .call(method::PROVIDERS_PROBE, json!({"provider": "mock"}))
        .await;
    let want = Capabilities {
        streaming: true,
        vision: true,
        tools: true,
        json_schema: true,
        embeddings: true,
        context_tokens: Some(4096),
    };
    assert_eq!(caps, want);
    let (c, _) = h
        .call_err(method::PROVIDERS_PROBE, json!({"provider": "needs-key"}))
        .await;
    assert_eq!(c, code::NOT_FOUND);
    let probes: Vec<_> = h
        .audit()
        .into_iter()
        .filter(|a| a.0 == "provider.probe")
        .collect();
    assert!(probes[0].2["error"].is_string(), "failed probe audited");
    assert_eq!(probes[1].1, "mock");
    assert_eq!(probes[1].2["checks"].as_array().unwrap().len(), 5);
    let egress: Vec<EgressEvent> = h.call(method::EGRESS_RECENT, json!({"limit": 10})).await;
    assert!(egress.is_empty(), "loopback probes are not egress");

    let mut h = h.restart(|_| {}).await;
    let list: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    assert_eq!(
        list[0].capabilities, None,
        "the failed probe stored nothing"
    );
    assert_eq!(list[1].capabilities.as_ref(), Some(&want));
    h.store.verify_audit_chain().unwrap();
    h.stop().await;
}
