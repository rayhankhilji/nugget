//! End-to-end tests over a real local socket.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nugget_core::ipc::{
    ChatDelta, ChatDone, ChatRequest, ChatStarted, Role, StatusResponse, method, notification,
};
use nugget_core::paths::Paths;
use nugget_ipc::{
    Client, Handler, IpcError, MAX_MESSAGE_BYTES, Notifier, RpcError, code, parse_params, to_result,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

const TIMEOUT: Duration = Duration::from_secs(20);

struct TestHandler;

#[async_trait]
impl Handler for TestHandler {
    async fn call(&self, method: &str, params: Value, notify: Notifier) -> Result<Value, RpcError> {
        match method {
            "echo" => Ok(params),
            method::STATUS => to_result(StatusResponse {
                protocol: nugget_core::ipc::PROTOCOL_VERSION,
                version: "test".into(),
                uptime_secs: 3,
                items: 10,
                chunks: 20,
                providers: vec![],
                audit_chain_ok: None,
            }),
            "sleep" => {
                let ms: u64 = parse_params::<Value>(params)?["ms"]
                    .as_u64()
                    .ok_or_else(|| RpcError::invalid_params("ms required"))?;
                tokio::time::sleep(Duration::from_millis(ms)).await;
                Ok(json!(ms))
            }
            "hang" => std::future::pending().await,
            method::CHAT => {
                let req: ChatRequest = parse_params(params)?;
                let stream_id = format!("s-{}", req.messages.len());
                let words: Vec<String> =
                    req.messages[0].text.split(' ').map(str::to_owned).collect();
                let sid = stream_id.clone();
                // Stream after the response, like the daemon will.
                tokio::spawn(async move {
                    for w in words {
                        let d = ChatDelta {
                            stream_id: sid.clone(),
                            text: w,
                        };
                        if notify.send(notification::CHAT_DELTA, &d).await.is_err() {
                            return;
                        }
                    }
                    let done = ChatDone {
                        stream_id: sid,
                        error: None,
                        input_tokens: Some(1),
                        output_tokens: Some(2),
                    };
                    let _ = notify.send(notification::CHAT_DONE, &done).await;
                });
                to_result(ChatStarted {
                    stream_id,
                    provider: nugget_core::ids::ProviderId::new("p"),
                })
            }
            "not_found" => Err(nugget_core::Error::NotFound("model x".into()).into()),
            "custom" => Err(RpcError::new(-32050, "custom").with_data(json!({"hint": 1}))),
            "panic" => panic!("boom"),
            "big_response" => Ok(Value::String("x".repeat(MAX_MESSAGE_BYTES))),
            "big_notification" => {
                let err = notify
                    .send("big", &"x".repeat(MAX_MESSAGE_BYTES))
                    .await
                    .unwrap_err();
                Ok(json!(matches!(err, IpcError::TooLarge(_))))
            }
            "wait_disconnect" => {
                // A detached task (like a chat stream) outlives the call; it must learn that
                // the client went away. The in-flight call itself is dropped on disconnect.
                let tx = WAIT_TX.lock().unwrap().take();
                tokio::spawn(async move {
                    notify.closed().await;
                    let err = notify.send("late", &()).await.unwrap_err();
                    if let Some(tx) = tx {
                        let _ = tx.send(matches!(err, IpcError::Disconnected));
                    }
                });
                std::future::pending().await
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

static WAIT_TX: std::sync::Mutex<Option<oneshot::Sender<bool>>> = std::sync::Mutex::new(None);

struct Fixture {
    _dir: tempfile::TempDir,
    paths: Paths,
    shutdown: Option<oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Fixture {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path().to_path_buf());
        let listener = nugget_ipc::bind(&paths).await.unwrap();
        let (tx, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(nugget_ipc::serve_until(
            listener,
            Arc::new(TestHandler),
            async {
                let _ = rx.await;
            },
        ));
        Self {
            _dir: dir,
            paths,
            shutdown: Some(tx),
            server,
        }
    }

    async fn client(&self) -> Client {
        Client::connect(&self.paths).await.unwrap()
    }

    async fn stop(&mut self) {
        let _ = self.shutdown.take().unwrap().send(());
        (&mut self.server).await.unwrap().unwrap();
    }
}

async fn within<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(TIMEOUT, f).await.expect("timed out")
}

#[tokio::test]
async fn round_trip() {
    let fx = Fixture::start().await;
    let client = fx.client().await;

    let v: Value = within(client.call("echo", &json!({"a": [1, 2, 3], "b": "ü"})))
        .await
        .unwrap();
    assert_eq!(v, json!({"a": [1, 2, 3], "b": "ü"}));

    let status: StatusResponse = within(client.call(method::STATUS, &())).await.unwrap();
    assert_eq!(status.items, 10);
    assert_eq!(status.chunks, 20);

    // A result that does not match the requested type is a decode error, not a disconnect.
    let err = within(client.call::<_, StatusResponse>("echo", &json!([1])))
        .await
        .unwrap_err();
    assert!(matches!(err, IpcError::Serde(_)), "{err:?}");
    assert!(!client.is_closed());
}

#[tokio::test]
async fn concurrent_calls_are_correlated_by_id() {
    let fx = Fixture::start().await;
    let client = fx.client().await;
    // Later calls finish first, so responses arrive out of order.
    let calls = (0..40u64).map(|i| {
        let c = client.clone();
        async move {
            let ms = (40 - i) * 5;
            let got: u64 = c.call("sleep", &json!({ "ms": ms })).await.unwrap();
            assert_eq!(got, ms);
        }
    });
    let started = std::time::Instant::now();
    within(futures::future::join_all(calls)).await;
    // Sequential would take ~4.1 s; concurrent is bounded by the slowest call (200 ms).
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn notifications_stream_to_the_calling_connection() {
    let fx = Fixture::start().await;
    let client = fx.client().await;
    let other = fx.client().await;
    let mut notes = client.notifications();
    let mut other_notes = other.notifications();

    let req = ChatRequest {
        provider: None,
        tier: nugget_core::capability::Tier::Light,
        messages: vec![nugget_core::ipc::ChatMessage {
            role: Role::User,
            text: "one two three".into(),
        }],
        items: Vec::new(),
    };
    let started: ChatStarted = within(client.call(method::CHAT, &req)).await.unwrap();

    let mut text = Vec::new();
    loop {
        let n = within(notes.next()).await.expect("stream ended");
        match n.method.as_str() {
            notification::CHAT_DELTA => {
                let d: ChatDelta = n.params_as().unwrap();
                assert_eq!(d.stream_id, started.stream_id);
                text.push(d.text);
            }
            notification::CHAT_DONE => {
                let d: ChatDone = n.params_as().unwrap();
                assert_eq!(d.stream_id, started.stream_id);
                assert_eq!(d.output_tokens, Some(2));
                break;
            }
            other => panic!("unexpected notification {other}"),
        }
    }
    assert_eq!(text, ["one", "two", "three"]);

    // Notifications are per connection.
    let none = tokio::time::timeout(Duration::from_millis(200), other_notes.next()).await;
    assert!(none.is_err(), "other connection received {none:?}");
}

#[tokio::test]
async fn errors_map_to_jsonrpc_codes() {
    let fx = Fixture::start().await;
    let client = fx.client().await;

    let rpc = |e: IpcError| match e {
        IpcError::Rpc(e) => e,
        other => panic!("expected rpc error, got {other:?}"),
    };

    let e = rpc(within(client.call_value("nope", Value::Null))
        .await
        .unwrap_err());
    assert_eq!(e.code, code::METHOD_NOT_FOUND);

    let e = rpc(within(client.call_value("sleep", json!({"ms": "x"})))
        .await
        .unwrap_err());
    assert_eq!(e.code, code::INVALID_PARAMS);

    let e = rpc(within(client.call_value("not_found", Value::Null))
        .await
        .unwrap_err());
    assert_eq!(e.code, code::NOT_FOUND);
    // And back into the shared error type, losslessly.
    match nugget_core::Error::from(e) {
        nugget_core::Error::NotFound(m) => assert_eq!(m, "model x"),
        other => panic!("{other:?}"),
    }

    let e = rpc(within(client.call_value("custom", Value::Null))
        .await
        .unwrap_err());
    assert_eq!((e.code, e.data), (-32050, Some(json!({"hint": 1}))));

    let e = rpc(within(client.call_value("panic", Value::Null))
        .await
        .unwrap_err());
    assert_eq!(e.code, code::INTERNAL_ERROR);

    // Scalar params are rejected client-side.
    let e = within(client.call_value("echo", json!(5)))
        .await
        .unwrap_err();
    assert!(matches!(e, IpcError::Protocol(_)), "{e:?}");

    // The connection survives all of the above.
    let v: Value = within(client.call("echo", &json!({"ok": true})))
        .await
        .unwrap();
    assert_eq!(v["ok"], true);
}

#[test]
fn core_errors_round_trip_through_rpc_codes() {
    use nugget_core::Error as E;
    let cases = [
        (E::NotFound("a".into()), code::NOT_FOUND),
        (E::Invalid("b".into()), code::INVALID_PARAMS),
        (E::Integrity("c".into()), code::INTEGRITY),
        (E::Unsupported("d".into()), code::UNSUPPORTED),
        (E::Provider("e".into()), code::PROVIDER),
        (E::Storage("f".into()), code::STORAGE),
        (E::Cancelled, code::CANCELLED),
        (E::Other("g".into()), code::SERVER_ERROR),
        (E::Io(std::io::Error::other("h")), code::IO),
    ];
    for (err, expected) in cases {
        let before = err.to_string();
        let rpc = RpcError::from(err);
        assert_eq!(rpc.code, expected, "{before}");
        let back = nugget_core::Error::from(rpc);
        if !matches!(back, E::Io(_)) {
            assert_eq!(back.to_string(), before);
        }
    }
}

/// Raw line-level access, to send what the typed client refuses to.
async fn raw_connect(
    paths: &Paths,
) -> (
    BufReader<tokio::io::ReadHalf<nugget_ipc::Stream>>,
    tokio::io::WriteHalf<nugget_ipc::Stream>,
) {
    let stream = nugget_ipc::connect_stream(paths).await.unwrap();
    let (r, w) = tokio::io::split(stream);
    (BufReader::new(r), w)
}

async fn read_json(r: &mut BufReader<tokio::io::ReadHalf<nugget_ipc::Stream>>) -> Option<Value> {
    let mut line = String::new();
    let n = within(r.read_line(&mut line)).await.unwrap();
    (n > 0).then(|| serde_json::from_str(&line).unwrap())
}

#[tokio::test]
async fn malformed_lines_get_error_responses() {
    let fx = Fixture::start().await;
    let (mut r, mut w) = raw_connect(&fx.paths).await;

    w.write_all(b"{oops\n").await.unwrap();
    let v = read_json(&mut r).await.unwrap();
    assert_eq!(v["error"]["code"], code::PARSE_ERROR);
    assert_eq!(v["id"], Value::Null);

    w.write_all(b"{\"id\":9,\"method\":\"echo\"}\n")
        .await
        .unwrap();
    let v = read_json(&mut r).await.unwrap();
    assert_eq!(v["error"]["code"], code::INVALID_REQUEST);
    assert_eq!(v["id"], 9);

    // String ids are echoed as-is.
    w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"abc\",\"method\":\"echo\",\"params\":[1]}\n")
        .await
        .unwrap();
    let v = read_json(&mut r).await.unwrap();
    assert_eq!(
        (v["id"].clone(), v["result"].clone()),
        (json!("abc"), json!([1]))
    );
}

#[tokio::test]
async fn oversized_messages_are_rejected() {
    let fx = Fixture::start().await;
    let client = fx.client().await;

    // Outgoing: refused locally, connection stays usable.
    let big = "x".repeat(MAX_MESSAGE_BYTES);
    let err = within(client.call_value("echo", json!([big])))
        .await
        .unwrap_err();
    assert!(
        matches!(err, IpcError::TooLarge(n) if n > MAX_MESSAGE_BYTES),
        "{err:?}"
    );

    // Response too large: replaced by an internal error.
    let err = within(client.call_value("big_response", Value::Null))
        .await
        .unwrap_err();
    assert_eq!(err.as_rpc().map(|e| e.code), Some(code::INTERNAL_ERROR));

    // Notification too large: the handler gets TooLarge.
    let ok: bool = within(client.call("big_notification", &())).await.unwrap();
    assert!(ok);
    assert!(!client.is_closed());

    // Incoming at the server: error response, then the connection is closed.
    let (mut r, mut w) = raw_connect(&fx.paths).await;
    let chunk = vec![b'a'; 1024 * 1024];
    for _ in 0..17 {
        if w.write_all(&chunk).await.is_err() {
            break; // the server may hang up before we finish
        }
    }
    let v = read_json(&mut r)
        .await
        .expect("error response before close");
    assert_eq!(v["error"]["code"], code::MESSAGE_TOO_LARGE);
    assert!(
        read_json(&mut r).await.is_none(),
        "connection should be closed"
    );
}

#[tokio::test]
async fn server_shutdown_fails_pending_calls_and_ends_streams() {
    let mut fx = Fixture::start().await;
    let client = fx.client().await;
    let mut notes = client.notifications();

    let pending = tokio::spawn({
        let c = client.clone();
        async move { c.call_value("hang", Value::Null).await }
    });
    // Make sure the call reached the server before shutting it down.
    let _: Value = within(client.call("echo", &json!({}))).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    fx.stop().await;

    let err = within(pending).await.unwrap().unwrap_err();
    assert!(matches!(err, IpcError::Disconnected), "{err:?}");
    within(client.closed()).await;
    assert!(client.is_closed());
    assert!(within(notes.next()).await.is_none());
    let err = client.call_value("echo", Value::Null).await.unwrap_err();
    assert!(err.is_disconnect());
    // New subscriptions on a closed client end immediately.
    assert!(within(client.notifications().next()).await.is_none());

    // Nothing is listening any more.
    let err = Client::connect(&fx.paths)
        .await
        .err()
        .expect("connect should fail");
    assert!(matches!(err, IpcError::Connect(_)), "{err:?}");
}

#[tokio::test]
async fn server_notices_client_disconnect() {
    let fx = Fixture::start().await;
    let (tx, rx) = oneshot::channel();
    *WAIT_TX.lock().unwrap() = Some(tx);
    let client = fx.client().await;
    let call = tokio::spawn({
        let c = client.clone();
        async move { c.call_value("wait_disconnect", Value::Null).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    call.abort();
    drop(client);
    assert!(
        within(rx).await.unwrap(),
        "notifier should report Disconnected"
    );
}

#[tokio::test]
async fn dropped_calls_do_not_leak_or_break_the_connection() {
    let fx = Fixture::start().await;
    let client = fx.client().await;
    let r = tokio::time::timeout(
        Duration::from_millis(50),
        client.call_value("sleep", json!({"ms": 300})),
    )
    .await;
    assert!(r.is_err());
    tokio::time::sleep(Duration::from_millis(400)).await; // the late response is discarded
    let v: Value = within(client.call("echo", &json!({"x": 1}))).await.unwrap();
    assert_eq!(v["x"], 1);
}

#[tokio::test]
async fn second_bind_fails_while_a_daemon_is_listening() {
    let fx = Fixture::start().await;
    let err = nugget_ipc::bind(&fx.paths)
        .await
        .expect_err("second bind should fail");
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    // The first server is unaffected.
    let v: Value = within(fx.client().await.call("echo", &json!([])))
        .await
        .unwrap();
    assert_eq!(v, json!([]));
}

#[cfg(unix)]
#[tokio::test]
async fn stale_socket_file_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(dir.path().to_path_buf());
    let path = std::path::PathBuf::from(nugget_ipc::socket_display(&paths));
    {
        // A std listener that is dropped without unlinking leaves a stale socket file behind.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _l = std::os::unix::net::UnixListener::bind(&path).unwrap();
    }
    assert!(path.exists());
    let listener = nugget_ipc::bind(&paths).await.unwrap();
    tokio::spawn(nugget_ipc::serve(listener, Arc::new(TestHandler)));
    let client = Client::connect(&paths).await.unwrap();
    let v: Value = within(client.call("echo", &json!({"ok": 1})))
        .await
        .unwrap();
    assert_eq!(v["ok"], 1);
}

/// Counts concurrent `block` calls and holds each until a permit is released, either by the
/// test or by an `agent.confirm` request (a control method in the default options) carrying
/// `{"permits": n}`, the way a confirmation releases a waiting agent run.
struct GateHandler {
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    gate: tokio::sync::Semaphore,
}

impl GateHandler {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active: 0.into(),
            peak: 0.into(),
            gate: tokio::sync::Semaphore::new(0),
        })
    }

    async fn wait_active(&self, n: usize) {
        use std::sync::atomic::Ordering::SeqCst;
        within(async {
            while self.active.load(SeqCst) < n {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
    }
}

#[async_trait]
impl Handler for GateHandler {
    async fn call(&self, method: &str, params: Value, _: Notifier) -> Result<Value, RpcError> {
        use std::sync::atomic::Ordering::SeqCst;
        match method {
            "block" => {
                let now = self.active.fetch_add(1, SeqCst) + 1;
                self.peak.fetch_max(now, SeqCst);
                self.gate.acquire().await.unwrap().forget();
                self.active.fetch_sub(1, SeqCst);
                Ok(Value::Null)
            }
            "echo" => Ok(params),
            nugget_core::ipc::agent_method::AGENT_CONFIRM => {
                let n = params["permits"].as_u64().unwrap_or(1);
                self.gate.add_permits(usize::try_from(n).unwrap());
                Ok(json!(n))
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

/// Serve `handler` with `options` on a fresh socket until the test ends.
async fn serve_gate(
    handler: Arc<GateHandler>,
    options: nugget_ipc::ServerOptions,
) -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(dir.path().to_path_buf());
    let listener = nugget_ipc::bind(&paths).await.unwrap();
    tokio::spawn(nugget_ipc::serve_with(
        listener,
        handler,
        options,
        std::future::pending(),
    ));
    (dir, paths)
}

fn is_busy(r: &Result<Value, IpcError>) -> bool {
    matches!(r, Err(IpcError::Rpc(e)) if e.code == code::SERVER_BUSY)
}

#[tokio::test]
async fn over_cap_requests_are_answered_busy_instead_of_waiting() {
    use std::sync::atomic::Ordering::SeqCst;
    const MAX: usize = 4;
    let handler = GateHandler::new();
    let options = nugget_ipc::ServerOptions::default().max_in_flight(MAX);
    let (_dir, paths) = serve_gate(handler.clone(), options).await;
    let client = Client::connect(&paths).await.unwrap();

    let calls: Vec<_> = (0..MAX)
        .map(|_| {
            let c = client.clone();
            tokio::spawn(async move { c.call_value("block", Value::Null).await })
        })
        .collect();
    handler.wait_active(MAX).await;

    // At the cap a further request is answered at once with a typed busy error (it used to
    // wait unread, unbounded in time), and nothing beyond the cap runs.
    let over = within(client.call_value("block", Value::Null)).await;
    assert!(is_busy(&over), "{over:?}");
    let err = over.unwrap_err();
    assert!(err.is_busy());
    assert_eq!(handler.peak.load(SeqCst), MAX);

    // Another connection has its own budget.
    let other = Client::connect(&paths).await.unwrap();
    let extra = tokio::spawn(async move { other.call_value("block", Value::Null).await });
    handler.wait_active(MAX + 1).await;

    // Freeing slots lets every running call finish, and the connection takes new calls again.
    handler.gate.add_permits(MAX + 1);
    for call in calls {
        within(call).await.unwrap().unwrap();
    }
    within(extra).await.unwrap().unwrap();
    let v = within(client.call_value("echo", json!([1]))).await.unwrap();
    assert_eq!(v, json!([1]));
    assert_eq!(handler.active.load(SeqCst), 0);
}

/// The deadlock this guards against: every slot is held by a call that waits for a later
/// request on the same connection. That request must still be read and run.
#[tokio::test]
async fn control_request_at_the_cap_releases_waiting_calls() {
    const MAX: usize = 4;
    let handler = GateHandler::new();
    let options = nugget_ipc::ServerOptions::default().max_in_flight(MAX);
    let (_dir, paths) = serve_gate(handler.clone(), options).await;
    let client = Client::connect(&paths).await.unwrap();

    let calls: Vec<_> = (0..MAX)
        .map(|_| {
            let c = client.clone();
            tokio::spawn(async move { c.call_value("block", Value::Null).await })
        })
        .collect();
    handler.wait_active(MAX).await;

    let confirm = nugget_core::ipc::agent_method::AGENT_CONFIRM;
    let released = within(client.call_value(confirm, json!({"permits": MAX})))
        .await
        .unwrap();
    assert_eq!(released, json!(MAX));
    for call in calls {
        within(call).await.unwrap().unwrap();
    }
}

/// A client flooding requests at a stuck connection gets one busy answer per extra request;
/// the server runs no more than the cap (plus the control reserve) at any time.
#[tokio::test]
async fn flooding_a_connection_stays_bounded() {
    use std::sync::atomic::Ordering::SeqCst;
    const MAX: usize = 2;
    const FLOOD: usize = 5_000;
    let handler = GateHandler::new();
    // `block` is made a control method here so the reserve itself is shown to be bounded.
    let options = nugget_ipc::ServerOptions::default()
        .max_in_flight(MAX)
        .control_methods(["block", "unused.control"]);
    let (_dir, paths) = serve_gate(handler.clone(), options).await;
    let (mut r, mut w) = raw_connect(&paths).await;

    let writer = tokio::spawn(async move {
        for id in 0..FLOOD {
            let line = format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"block\"}}\n");
            w.write_all(line.as_bytes()).await.unwrap();
        }
        w
    });
    let limit = MAX + nugget_ipc::CONTROL_RESERVE;
    let mut busy = 0;
    while busy < FLOOD - limit {
        let resp = read_json(&mut r).await.expect("connection was dropped");
        assert_eq!(resp["error"]["code"], code::SERVER_BUSY, "{resp}");
        busy += 1;
    }
    let mut w = within(writer).await.unwrap();
    handler.wait_active(limit).await;
    assert_eq!(handler.peak.load(SeqCst), limit);

    // Release them all: exactly `limit` successful responses follow.
    handler.gate.add_permits(limit);
    for _ in 0..limit {
        let resp = read_json(&mut r).await.expect("connection was dropped");
        assert_eq!(resp["result"], Value::Null, "{resp}");
    }
    // Still usable afterwards.
    w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"e\",\"method\":\"echo\",\"params\":[2]}\n")
        .await
        .unwrap();
    let resp = read_json(&mut r).await.unwrap();
    assert_eq!(resp["result"], json!([2]));
}

#[test]
fn busy_errors_are_typed_and_map_to_core_errors() {
    let e = RpcError::busy(4);
    assert_eq!(e.code, code::SERVER_BUSY);
    assert_eq!(e.data, Some(json!({"max_in_flight": 4})));
    assert!(IpcError::Rpc(e.clone()).is_busy());
    assert!(!IpcError::Disconnected.is_busy());
    assert!(matches!(
        nugget_core::Error::from(e),
        nugget_core::Error::RateLimited {
            retry_after_ms: None
        }
    ));
}

/// The server's peer-credential check admits a client running as the same user.
#[tokio::test]
async fn same_user_peer_passes_the_credential_check() {
    let fx = Fixture::start().await;
    let (mut r, mut w) = raw_connect(&fx.paths).await;
    w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"echo\",\"params\":[7]}\n")
        .await
        .unwrap();
    let resp = read_json(&mut r).await.expect("connection was dropped");
    assert_eq!(resp["result"], json!([7]));
}

/// Micro-benchmark over the real local transport. Run with
/// `cargo test -p nugget-ipc --release --test ipc -- --ignored --nocapture bench_small_calls`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark; prints numbers"]
async fn bench_small_calls() {
    use std::time::Instant;
    const SEQ: usize = 20_000;
    const TASKS: usize = 32;
    const PER_TASK: usize = 2_000;
    let fx = Fixture::start().await;
    let client = fx.client().await;
    let params = json!({"q": "hello", "k": 5});
    for _ in 0..1_000 {
        client.call_value("echo", params.clone()).await.unwrap();
    }

    let mut lat = Vec::with_capacity(SEQ);
    let start = Instant::now();
    for _ in 0..SEQ {
        let t = Instant::now();
        client.call_value("echo", params.clone()).await.unwrap();
        lat.push(t.elapsed());
    }
    let seq = start.elapsed();
    lat.sort();
    println!(
        "sequential: {:.0} req/s, p50 {:?}, p99 {:?}",
        SEQ as f64 / seq.as_secs_f64(),
        lat[SEQ / 2],
        lat[SEQ * 99 / 100]
    );

    let start = Instant::now();
    let tasks: Vec<_> = (0..TASKS)
        .map(|_| {
            let c = client.clone();
            let p = params.clone();
            tokio::spawn(async move {
                for _ in 0..PER_TASK {
                    c.call_value("echo", p.clone()).await.unwrap();
                }
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    let total = TASKS * PER_TASK;
    println!(
        "{TASKS} concurrent callers, one connection: {:.0} req/s",
        total as f64 / start.elapsed().as_secs_f64()
    );
}
