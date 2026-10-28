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
