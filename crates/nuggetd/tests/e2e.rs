//! End-to-end: run the daemon in-process on a temporary home, talk to it with the real IPC
//! client, and serve model traffic from a wiremock OpenAI-compatible server (plus a raw TCP
//! server for a slow, never-ending stream).
//!
//! The store key is passed in explicitly, so no OS keychain is touched (the CLI test covers the
//! `NUGGET_KEYCHAIN=off` key-file path through the real binary).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use futures::{Stream, StreamExt};
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::egress::EgressEvent;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    ChatCancel, ChatDelta, ChatDone, ChatMessage, ChatRequest, ChatStarted, EgressRecentRequest,
    ModelInfo, ModelInstallRequest, ProbeRequest, ProviderInfo, Role, SearchHit, SearchRequest,
    StatusResponse, method, notification,
};
use nugget_core::paths::Paths;
use nugget_infer::probe::vision::read_code_png;
use nugget_ipc::{Client, IpcError, Notification, code};
use nugget_store::{NewChunk, NewItem, Store, StoreKey, content_hash};
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use wiremock::matchers::method as http_method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const WAIT: Duration = Duration::from_secs(15);

// ---- a capable OpenAI-compatible mock ----

fn sse_body(chunks: &[Value]) -> String {
    let mut s: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    s.push_str("data: [DONE]\n\n");
    s
}

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
            .set_body_string(sse_body(&chunks))
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

// ---- harness ----

struct Harness {
    _tmp: tempfile::TempDir,
    paths: Paths,
    key: StoreKey,
    store: Store,
    client: Client,
    mock: MockServer,
    slow: SlowServer,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<nugget_core::Result<()>>>,
}

fn config(mock: &MockServer, slow: &SlowServer) -> String {
    format!(
        r#"
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

# Cannot be built (its key variable is unset): listed with an error, the daemon still starts.
[[providers]]
id = "needs-key"
kind = "anthropic"
model = "m"
api_key = "env:NUGGETD_TEST_DEFINITELY_UNSET_KEY"
tiers = ["synthesis"]
"#,
        mock.uri(),
        slow.addr
    )
}

async fn start_daemon(
    paths: &Paths,
    key: &StoreKey,
) -> (
    Store,
    oneshot::Sender<()>,
    JoinHandle<nugget_core::Result<()>>,
) {
    let opts = DaemonOptions {
        paths: paths.clone(),
        store_key: Some(key.clone()),
    };
    let daemon = match Daemon::start(opts).await.unwrap() {
        Startup::Ready(d) => d,
        Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
    };
    let store = daemon.store();
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(daemon.run_until(async {
        let _ = rx.await;
    }));
    (store, tx, server)
}

impl Harness {
    async fn start() -> Self {
        Self::start_with(config).await
    }

    async fn start_with(config: impl Fn(&MockServer, &SlowServer) -> String) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path().to_path_buf());
        let mock = MockServer::start().await;
        Mock::given(http_method("POST"))
            .respond_with(SmartModel)
            .mount(&mock)
            .await;
        let slow = slow_server().await;
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config_file(), config(&mock, &slow)).unwrap();
        let key = StoreKey::from_bytes([7; 32]);
        let (store, stop, server) = start_daemon(&paths, &key).await;
        let client = Client::connect(&paths).await.unwrap();
        Self {
            _tmp: tmp,
            paths,
            key,
            store,
            client,
            mock,
            slow,
            stop: Some(stop),
            server: Some(server),
        }
    }

    async fn call<R: serde::de::DeserializeOwned>(&self, m: &str, params: Value) -> R {
        tokio::time::timeout(WAIT, self.client.call(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} failed: {e}"))
    }

    async fn call_err(&self, m: &str, params: Value) -> i64 {
        match self.client.call_value(m, params).await {
            Err(IpcError::Rpc(e)) => e.code,
            other => panic!("{m}: expected an RPC error, got {other:?}"),
        }
    }

    async fn stop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(WAIT, self.server.take().unwrap())
            .await
            .expect("daemon stops promptly")
            .unwrap()
            .unwrap();
    }

    async fn restart(&mut self) {
        let (store, stop, server) = start_daemon(&self.paths, &self.key).await;
        self.store = store;
        self.stop = Some(stop);
        self.server = Some(server);
        self.client = Client::connect(&self.paths).await.unwrap();
    }
}

fn chat_request(provider: Option<&str>, tier: Tier, text: &str) -> Value {
    serde_json::to_value(ChatRequest {
        provider: provider.map(ProviderId::new),
        tier,
        messages: vec![
            ChatMessage {
                role: Role::System,
                text: "You are terse.".into(),
            },
            ChatMessage {
                role: Role::User,
                text: text.into(),
            },
        ],
    })
    .unwrap()
}

/// Collect one stream's deltas until its `chat.done`.
async fn collect_chat(
    notes: &mut (impl Stream<Item = Notification> + Unpin),
    stream_id: &str,
) -> (String, ChatDone) {
    let mut text = String::new();
    loop {
        let n = tokio::time::timeout(WAIT, notes.next())
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

/// Poll `status` until the startup audit-chain check has finished, and return its result.
async fn wait_audit_check(h: &Harness) -> bool {
    tokio::time::timeout(WAIT, async {
        loop {
            let s: StatusResponse = h.call(method::STATUS, json!({})).await;
            if let Some(ok) = s.audit_chain_ok {
                return ok;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("audit chain checked at startup")
}

/// A provider nothing listens on (the discard port), ahead of the mock in the `light` tier.
fn config_with_dead_provider(mock: &MockServer, _slow: &SlowServer) -> String {
    format!(
        r#"
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
tiers = ["light"]
max_retries = 0
"#,
        mock.uri()
    )
}

// ---- tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_chat_search_egress_and_models() {
    let mut h = Harness::start().await;

    // Default config is not written over an existing one.
    assert!(
        std::fs::read_to_string(h.paths.config_file())
            .unwrap()
            .contains("slow-model")
    );

    // Seed the store directly.
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
    let other =
        NewItem::new("files:test", "notes/cats.md", content_hash("cats")).with_title("Cats");
    h.store.upsert_item(&other).unwrap();
    h.store
        .replace_chunks(&other.item, &[NewChunk::text("Cats sleep a lot.")])
        .unwrap();

    // status
    let status: StatusResponse = h.call(method::STATUS, json!({})).await;
    assert_eq!(status.protocol, nugget_core::ipc::PROTOCOL_VERSION);
    assert_eq!(status.version, env!("CARGO_PKG_VERSION"));
    assert_eq!((status.items, status.chunks), (2, 3));
    assert!(status.uptime_secs < 60);
    let ids: Vec<&str> = status.providers.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, vec!["mock", "slow", "needs-key"], "config order");
    let mock = &status.providers[0];
    assert_eq!(mock.kind, "openai_compatible");
    assert_eq!(mock.model, "test-model");
    assert_eq!(mock.locality, Locality::OnDevice);
    assert_eq!(mock.tiers, vec![Tier::Light, Tier::Synthesis]);
    assert_eq!(mock.capabilities, None);
    assert_eq!(mock.error, None);
    // The unbuildable provider is listed with why, instead of vanishing.
    let broken = &status.providers[2];
    assert_eq!(broken.kind, "anthropic");
    assert_eq!(broken.model, "m");
    assert_eq!(broken.locality, Locality::Cloud);
    assert_eq!(broken.tiers, vec![Tier::Synthesis]);
    assert_eq!(broken.capabilities, None);
    let reason = broken.error.as_deref().expect("build error reported");
    assert!(
        reason.contains("NUGGETD_TEST_DEFINITELY_UNSET_KEY"),
        "{reason}"
    );
    // The audit chain is verified at startup (in the background).
    assert!(wait_audit_check(&h).await, "audit chain intact");
    // `null` params work too (what `Client::call(_, &())` sends).
    let again: StatusResponse = h.call(method::STATUS, Value::Null).await;
    assert_eq!(again.providers, status.providers);
    let list: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    assert_eq!(list, status.providers);

    // chat routed by tier: deltas, then done with usage.
    let mut notes = h.client.notifications();
    let started: ChatStarted = h
        .call(
            method::CHAT,
            chat_request(None, Tier::Synthesis, "say hello"),
        )
        .await;
    assert_eq!(started.provider.as_str(), "mock");
    let (text, done) = collect_chat(&mut notes, &started.stream_id).await;
    assert_eq!(text, "Hello");
    assert_eq!(done.error, None);
    assert_eq!(done.input_tokens, Some(9));
    assert_eq!(done.output_tokens, Some(2));

    // The provider saw the system prompt and the user turn.
    let reqs = h.mock.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&reqs.last().unwrap().body).unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "You are terse.");
    assert_eq!(body["messages"][1]["content"], "say hello");

    // An explicit provider wins over the tier.
    let started: ChatStarted = h
        .call(
            method::CHAT,
            chat_request(Some("mock"), Tier::Triage, "ping"),
        )
        .await;
    assert_eq!(started.provider.as_str(), "mock");
    let (text, done) = collect_chat(&mut notes, &started.stream_id).await;
    assert_eq!((text.as_str(), done.error), ("pong", None));

    // Routing and parameter errors are RPC errors.
    assert_eq!(
        h.call_err(method::CHAT, chat_request(None, Tier::Embedding, "x"))
            .await,
        code::NOT_FOUND
    );
    assert_eq!(
        h.call_err(method::CHAT, chat_request(Some("nope"), Tier::Light, "x"))
            .await,
        code::NOT_FOUND
    );
    assert_eq!(
        h.call_err(
            method::CHAT,
            chat_request(Some("needs-key"), Tier::Light, "x")
        )
        .await,
        code::NOT_FOUND
    );
    assert_eq!(
        h.call_err(
            method::CHAT,
            json!({"provider": null, "tier": "light", "messages": []})
        )
        .await,
        code::INVALID_PARAMS
    );
    assert_eq!(
        h.call_err(method::CHAT, json!({"tier": "bogus"})).await,
        code::INVALID_PARAMS
    );
    assert_eq!(
        h.call_err("no.such.method", json!({})).await,
        code::METHOD_NOT_FOUND
    );

    // search over the seeded items.
    let hits: Vec<SearchHit> = h
        .call(
            method::SEARCH,
            json!(SearchRequest {
                query: "fox".into(),
                limit: 10
            }),
        )
        .await;
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|hit| hit.item == item.item));
    assert_eq!(hits[0].title.as_deref(), Some("Fox notes"));
    assert!(hits[0].chunk_id.is_some());
    assert!(hits[0].score >= hits[1].score);
    let hits: Vec<SearchHit> = h
        .call(method::SEARCH, json!({"query": "cats", "limit": 1}))
        .await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item, other.item);
    let none: Vec<SearchHit> = h
        .call(method::SEARCH, json!({"query": "fox", "limit": 0}))
        .await;
    assert!(none.is_empty());

    // The loopback provider never shows up in the egress ledger.
    let egress: Vec<EgressEvent> = h
        .call(
            method::EGRESS_RECENT,
            json!(EgressRecentRequest { limit: 50 }),
        )
        .await;
    assert!(egress.is_empty(), "{egress:?}");

    // models.list: the bundled manifest, nothing installed.
    let manifest = nugget_models::default_manifest().unwrap();
    let models: Vec<ModelInfo> = h.call(method::MODELS_LIST, json!({})).await;
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    let want: Vec<&str> = manifest.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, want);
    for (info, entry) in models.iter().zip(&manifest.models) {
        assert_eq!(info.size_bytes, entry.size);
        assert_eq!(info.family, entry.family);
        assert_eq!(info.tiers, entry.tiers);
        assert_eq!(info.license, entry.license);
        assert_eq!(info.revision, entry.revision);
        assert!(!info.installed && !info.update_available);
    }
    let hw = nugget_models::detect_hardware();
    for tier in [Tier::Triage, Tier::Synthesis, Tier::Embedding] {
        if let Some(rec) = nugget_models::recommend(&manifest, &hw, tier) {
            assert!(models.iter().any(|m| m.id == rec.id && m.recommended));
        }
    }
    assert_eq!(
        h.call_err(
            method::MODELS_INSTALL,
            json!(ModelInstallRequest {
                id: "no-such-model".into()
            })
        )
        .await,
        code::NOT_FOUND
    );

    // Chats are audited, without message content.
    let audit = h.store.recent_audit(50).unwrap();
    let chats: Vec<_> = audit.iter().filter(|a| a.action == "chat").collect();
    assert_eq!(chats.len(), 2);
    assert!(!chats[0].detail.to_string().contains("say hello"));
    h.store.verify_audit_chain().unwrap();

    h.stop().await;
    assert!(
        Client::connect(&h.paths).await.is_err(),
        "socket closed after shutdown"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_cancel_stops_a_slow_stream() {
    let mut h = Harness::start().await;
    let mut notes = h.client.notifications();

    let started: ChatStarted = h
        .call(method::CHAT, chat_request(None, Tier::Agentic, "go slowly"))
        .await;
    assert_eq!(started.provider.as_str(), "slow");

    // Wait for the stream to be flowing.
    let mut seen = 0;
    while seen < 2 {
        let n = tokio::time::timeout(WAIT, notes.next())
            .await
            .unwrap()
            .unwrap();
        if n.method == notification::CHAT_DELTA {
            let d: ChatDelta = n.params_as().unwrap();
            assert_eq!(d.stream_id, started.stream_id);
            seen += 1;
        }
    }

    let () = h
        .call(
            method::CHAT_CANCEL,
            json!(ChatCancel {
                stream_id: started.stream_id.clone()
            }),
        )
        .await;
    let (_, done) = collect_chat(&mut notes, &started.stream_id).await;
    assert_eq!(done.error.as_deref(), Some("cancelled"));

    // No more deltas for that stream, and the HTTP stream to the provider was dropped.
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
    tokio::time::timeout(WAIT, async {
        while !h.slow.closed.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("provider connection closed after cancel");
    assert_eq!(h.slow.requests.load(Ordering::SeqCst), 1);

    // Cancelling again (or an unknown stream) is a no-op.
    let () = h
        .call(method::CHAT_CANCEL, json!({"stream_id": started.stream_id}))
        .await;
    let () = h
        .call(method::CHAT_CANCEL, json!({"stream_id": "unknown"}))
        .await;

    // A slow stream whose client goes away is stopped too.
    let client2 = Client::connect(&h.paths).await.unwrap();
    let mut notes2 = client2.notifications();
    let started: ChatStarted = client2
        .call(
            method::CHAT,
            &chat_request(Some("slow"), Tier::Light, "again"),
        )
        .await
        .unwrap();
    let first = tokio::time::timeout(WAIT, notes2.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.method, notification::CHAT_DELTA);
    h.slow.closed.store(false, Ordering::SeqCst);
    drop(notes2);
    drop(client2);
    tokio::time::timeout(WAIT, async {
        while !h.slow.closed.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("stream {} kept running after disconnect", started.stream_id));

    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_is_persisted_and_routes_capability_requests() {
    let mut h = Harness::start().await;

    let caps: Capabilities = h
        .call(
            method::PROVIDERS_PROBE,
            json!(ProbeRequest {
                provider: ProviderId::new("mock")
            }),
        )
        .await;
    assert_eq!(
        caps,
        Capabilities {
            streaming: true,
            vision: true,
            tools: true,
            json_schema: true,
            embeddings: true,
            context_tokens: Some(4096),
        }
    );
    assert_eq!(
        h.call_err(method::PROVIDERS_PROBE, json!({"provider": "needs-key"}))
            .await,
        code::NOT_FOUND
    );

    let list: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    assert_eq!(list[0].capabilities.as_ref(), Some(&caps));
    assert_eq!(list[1].capabilities, None);
    assert!(list[2].error.is_some());

    // Probes are audited and the result survives a restart.
    let audit = h.store.recent_audit(10).unwrap();
    let probe = audit
        .iter()
        .find(|a| a.action == "provider.probe")
        .expect("probe audited");
    assert_eq!(probe.target, "mock");
    assert_eq!(probe.detail["checks"].as_array().unwrap().len(), 5);
    assert!(
        nuggetd::probes::ProbeCache::path_for(&h.paths).exists(),
        "probe results persisted"
    );

    // Probe traffic to a loopback provider is not egress either.
    let egress: Vec<EgressEvent> = h.call(method::EGRESS_RECENT, json!({"limit": 10})).await;
    assert!(egress.is_empty());

    h.stop().await;
    h.restart().await;
    let list: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    assert_eq!(list[0].id.as_str(), "mock");
    assert_eq!(list[0].capabilities.as_ref(), Some(&caps));
    h.store.verify_audit_chain().unwrap();
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_daemon_on_the_same_home_reports_already_running() {
    let mut h = Harness::start().await;
    let opts = DaemonOptions {
        paths: h.paths.clone(),
        store_key: Some(h.key.clone()),
    };
    match Daemon::start(opts).await.unwrap() {
        Startup::AlreadyRunning { socket } => {
            assert_eq!(socket, nugget_ipc::socket_display(&h.paths));
        }
        Startup::Ready(_) => panic!("second daemon must not start"),
    }
    // The first one is unaffected.
    let s: StatusResponse = h.call(method::STATUS, json!({})).await;
    assert_eq!(s.providers.len(), 3);
    h.stop().await;
}

#[tokio::test]
async fn invalid_config_is_a_clear_error() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path().to_path_buf());
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(
        paths.config_file(),
        "[[providers]]\nid = \"x\"\nkind = \"carrier_pigeon\"\nmodel = \"m\"\n",
    )
    .unwrap();
    let err = match Daemon::start(DaemonOptions {
        paths: paths.clone(),
        store_key: Some(StoreKey::from_bytes([1; 32])),
    })
    .await
    {
        Err(e) => e.to_string(),
        Ok(_) => panic!("invalid config must not start"),
    };
    assert!(err.contains("config.toml"), "{err}");
    assert!(err.contains("carrier_pigeon"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_falls_back_when_the_first_provider_is_down() {
    let mut h = Harness::start_with(config_with_dead_provider).await;
    let mut notes = h.client.notifications();

    // The first chat after an outage is served by the next provider, and `ChatStarted` names the
    // provider that actually serves it.
    let started: ChatStarted = h
        .call(method::CHAT, chat_request(None, Tier::Light, "say hello"))
        .await;
    assert_eq!(started.provider.as_str(), "mock");
    let (text, done) = collect_chat(&mut notes, &started.stream_id).await;
    assert_eq!((text.as_str(), done.error), ("Hello", None));

    // The audit names the serving provider too.
    let audit = h.store.recent_audit(10).unwrap();
    let chat = audit.iter().find(|a| a.action == "chat").unwrap();
    assert_eq!(chat.target, "mock");

    // An explicit provider is never substituted: a dead one is an error.
    assert_eq!(
        h.call_err(method::CHAT, chat_request(Some("dead"), Tier::Light, "x"))
            .await,
        code::PROVIDER
    );

    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probing_an_unreachable_provider_records_nothing() {
    let mut h = Harness::start_with(config_with_dead_provider).await;

    let err = h
        .client
        .call_value(method::PROVIDERS_PROBE, json!({"provider": "dead"}))
        .await;
    match err {
        Err(IpcError::Rpc(e)) => {
            assert_eq!(e.code, code::PROVIDER);
            assert!(e.message.contains("unreachable"), "{}", e.message);
        }
        other => panic!("expected an RPC error, got {other:?}"),
    }

    // Nothing persisted or recorded: the provider still counts as unprobed, so plain chat is
    // still allowed once it is back.
    assert!(!nuggetd::probes::ProbeCache::path_for(&h.paths).exists());
    let list: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    assert_eq!(list[0].id.as_str(), "dead");
    assert_eq!(list[0].capabilities, None);
    let audit = h.store.recent_audit(10).unwrap();
    let probe = audit
        .iter()
        .find(|a| a.action == "provider.probe")
        .expect("failed probe audited");
    assert!(probe.detail["error"].is_string());

    // Restarting does not pick up any stored capabilities either.
    h.stop().await;
    h.restart().await;
    let list: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    assert_eq!(list[0].capabilities, None);
    let started: ChatStarted = h
        .call(method::CHAT, chat_request(None, Tier::Light, "ping"))
        .await;
    assert_eq!(started.provider.as_str(), "mock");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_ends_open_chats_with_chat_done() {
    let mut h = Harness::start().await;
    let mut notes = h.client.notifications();

    let started: ChatStarted = h
        .call(method::CHAT, chat_request(None, Tier::Agentic, "go slowly"))
        .await;
    assert_eq!(started.provider.as_str(), "slow");
    let first = tokio::time::timeout(WAIT, notes.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.method, notification::CHAT_DELTA);

    let _ = h.stop.take().unwrap().send(());
    let (_, done) = collect_chat(&mut notes, &started.stream_id).await;
    assert_eq!(done.error.as_deref(), Some("nuggetd is shutting down"));
    tokio::time::timeout(WAIT, h.server.take().unwrap())
        .await
        .expect("daemon stops promptly")
        .unwrap()
        .unwrap();
}
