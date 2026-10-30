//! Double Tap through IPC (`draft.*`, `voice.status`), against a fake model server: streaming,
//! cancellation, shutdown, feedback, the sensitive gate (nothing reaches the model), excluded
//! apps, prewarm, the voice profile at startup and its refresh after an ingest commit.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use nugget_core::ipc::{ChatDelta, ChatDone, DraftStarted, VoiceStatus, method, notification};
use nugget_core::paths::Paths;
use nugget_ingest::MemorySecrets;
use nugget_ipc::{Client, IpcError, Notification, RpcError, code};
use nugget_store::{NewChunk, NewItem, Store, StoreKey, content_hash};
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(20);
const KEY: [u8; 32] = [7; 32];
/// Put in a request (as the instruction) to make the fake model send one delta and then hang.
const SLOW: &str = "SLOWMODEPLEASE";
/// Field contents that must never reach the model or the audit log.
const FIELD_SECRET: &str = "hunter2-TOPSECRET";
const THREAD_SECRET: &str = "Thursday-CODEWORD";

// ---- a fake OpenAI-compatible server (HTTP/1.1, keep-alive) ----

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    body: String,
}

#[derive(Clone, Default)]
struct FakeModel {
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeModel {
    async fn start() -> (Self, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let me = Self::default();
        let server = me.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(server.clone().serve(sock));
            }
        });
        (me, format!("http://{addr}/v1"))
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn chats(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.method == "POST")
            .collect()
    }

    async fn serve(self, mut sock: TcpStream) {
        let mut buf = Vec::new();
        loop {
            let Some((head_len, content_len)) = read_head(&mut sock, &mut buf).await else {
                return;
            };
            while buf.len() < head_len + content_len {
                let mut chunk = [0u8; 8192];
                match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let head = String::from_utf8_lossy(&buf[..head_len]).into_owned();
            let body = String::from_utf8_lossy(&buf[head_len..head_len + content_len]).into_owned();
            buf.drain(..head_len + content_len);
            let mut first = head.lines().next().unwrap_or("").split(' ');
            let seen = Seen {
                method: first.next().unwrap_or("").to_owned(),
                path: first.next().unwrap_or("").to_owned(),
                body,
            };
            self.seen.lock().unwrap().push(seen.clone());

            if seen.method != "POST" {
                let b = r#"{"object":"list","data":[]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{b}",
                    b.len()
                );
                if sock.write_all(resp.as_bytes()).await.is_err() {
                    return;
                }
                continue;
            }
            let delta = |t: &str| {
                format!(
                    "data: {}\n\n",
                    json!({"choices": [{"index": 0, "delta": {"content": t}}]})
                )
            };
            if seen.body.contains(SLOW) {
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
                let first = delta("Sounds good, ");
                let chunk = format!("{:x}\r\n{first}\r\n", first.len());
                if sock.write_all(head.as_bytes()).await.is_err()
                    || sock.write_all(chunk.as_bytes()).await.is_err()
                {
                    return;
                }
                // Hang until the client goes away.
                let mut b = [0u8; 1024];
                while matches!(sock.read(&mut b).await, Ok(n) if n > 0) {}
                return;
            }
            let mut body = delta("Thursday works for me, ");
            body.push_str(&delta("see you then."));
            body.push_str(&format!(
                "data: {}\n\n",
                json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
            ));
            body.push_str("data: [DONE]\n\n");
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            if sock.write_all(resp.as_bytes()).await.is_err() {
                return;
            }
        }
    }
}

/// Read until the end of the headers; returns (header length, content length).
async fn read_head(sock: &mut TcpStream, buf: &mut Vec<u8>) -> Option<(usize, usize)> {
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            return Some((i + 4, len));
        }
        let mut chunk = [0u8; 8192];
        match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

// ---- the daemon ----

struct Harness {
    tmp: tempfile::TempDir,
    model: FakeModel,
    store: Store,
    client: Client,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<nugget_core::Result<()>>>,
}

/// `draft` is TOML for the `[draft]` section; `seed` runs on the store before the daemon starts.
async fn start(draft: &str, seed: impl FnOnce(&Store)) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path().to_path_buf());
    let (model, base) = FakeModel::start().await;
    std::fs::create_dir_all(&paths.config).unwrap();
    {
        let db = paths.db_file();
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = Store::open(&db, &StoreKey::from_bytes(KEY)).unwrap();
        seed(&store);
    }
    let config = format!(
        r#"[runtime]
enabled = false

[[providers]]
id = "fake"
kind = "openai_compatible"
base_url = "{base}"
model = "fake-model"
tiers = ["synthesis"]
max_retries = 0
# Loopback, but treated as cloud so its traffic is in the egress ledger.
locality = "cloud"

[draft]
voice_refresh_debounce_ms = 100
{draft}
"#
    );
    std::fs::write(paths.config_file(), config).unwrap();
    let mut opts = DaemonOptions::new(paths.clone());
    opts.store_key = Some(StoreKey::from_bytes(KEY));
    opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
    let daemon = match Daemon::start(opts).await.unwrap() {
        Startup::Ready(d) => d,
        Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
    };
    let store = daemon.store();
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(daemon.run_until(async {
        let _ = rx.await;
    }));
    let client = Client::connect(&paths).await.unwrap();
    Harness {
        tmp,
        model,
        store,
        client,
        stop: Some(tx),
        server: Some(server),
    }
}

impl Harness {
    async fn call<R: serde::de::DeserializeOwned>(&self, m: &str, params: Value) -> R {
        tokio::time::timeout(WAIT, self.client.call(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} failed: {e}"))
    }

    async fn call_err(&self, m: &str, params: Value) -> RpcError {
        let r: Result<Value, IpcError> = tokio::time::timeout(WAIT, self.client.call(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"));
        match r {
            Ok(v) => panic!("{m} succeeded: {v}"),
            Err(e) => e.as_rpc().cloned().unwrap_or_else(|| panic!("{m}: {e}")),
        }
    }

    /// Every audit record's action and detail, oldest first.
    fn audit(&self) -> Vec<(String, String, Value)> {
        let mut v: Vec<_> = self
            .store
            .recent_audit(1000)
            .unwrap()
            .into_iter()
            .map(|r| (r.action, r.target, r.detail))
            .collect();
        v.reverse();
        v
    }

    async fn audit_eventually(&self, action: &str, n: usize) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let found: Vec<Value> = self
                .audit()
                .into_iter()
                .filter(|(a, _, _)| a == action)
                .map(|(_, _, d)| d)
                .collect();
            if found.len() >= n || tokio::time::Instant::now() > deadline {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
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
}

/// Text and end of one stream.
async fn collect(
    notes: &mut (impl futures::Stream<Item = Notification> + Unpin),
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

/// The first delta of a stream.
async fn first_delta(
    notes: &mut (impl futures::Stream<Item = Notification> + Unpin),
    stream_id: &str,
) -> String {
    loop {
        let n = tokio::time::timeout(WAIT, notes.next())
            .await
            .expect("chat notification in time")
            .expect("connection open");
        if n.method == notification::CHAT_DELTA {
            let d: ChatDelta = n.params_as().unwrap();
            if d.stream_id == stream_id {
                return d.text;
            }
        }
        assert_ne!(n.method, notification::CHAT_DONE, "stream ended early");
    }
}

/// A Slack reply box with a short thread.
fn chat_context() -> Value {
    json!({
        "app": {"id": "com.tinyspeck.slackmacgap", "name": "Slack"},
        "field": {"role": "text_area", "placeholder": "Message #general", "value": ""},
        "thread": [
            {"author": "Sam", "text": format!("Can we move the review to {THREAD_SECRET}?"), "from_me": false}
        ],
        "recipients": ["Sam"]
    })
}

fn chat_item(store: &Store, key: &str, text: &str) {
    let meta = json!({"source": {"from_me": true, "channel": "general"}, "from_me": true});
    let item = NewItem::new("chat-1", key, content_hash(text))
        .with_title(key)
        .with_text(text)
        .with_mime("text/plain")
        .with_version(format!("v1:{}", content_hash(text)))
        .with_metadata(meta);
    store
        .upsert_item_with_chunks(&item, &[NewChunk::text(text)])
        .unwrap();
}

fn seed_chat(store: &Store, from: usize, to: usize) {
    for i in from..to {
        chat_item(
            store,
            &format!("m{i}"),
            &format!("sure thing, will do {i} later today"),
        );
    }
}

async fn voice_status_eventually(h: &Harness, pred: impl Fn(&VoiceStatus) -> bool) -> VoiceStatus {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let s: VoiceStatus = h.call(method::VOICE_STATUS, json!({})).await;
        if pred(&s) || tokio::time::Instant::now() > deadline {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---- tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn begin_streams_the_draft_then_feedback_is_recorded() {
    let mut h = start("", |_| {}).await;
    let mut notes = h.client.notifications();

    let started: DraftStarted = h
        .call(method::DRAFT_BEGIN, json!({"context": chat_context()}))
        .await;
    assert_eq!(started.provider.as_str(), "fake");
    assert_eq!(started.field_kind, "chat_message");
    let (text, done) = collect(&mut notes, &started.stream_id).await;
    assert_eq!(done.error, None);
    assert!(text.contains("Thursday works for me"), "{text:?}");
    assert!(!text.ends_with('\n'));

    // The thread was sent to the model, under the `draft` egress feature.
    let chats = h.model.chats();
    assert_eq!(chats.len(), 1);
    assert!(chats[0].body.contains(THREAD_SECRET));
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        h.store.flush_egress();
        let events: Vec<nugget_core::egress::EgressEvent> =
            h.call(method::EGRESS_RECENT, json!({"limit": 50})).await;
        if events.iter().any(|e| e.feature == "draft") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{events:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Feedback: every outcome is accepted, also for an unknown stream; a bad outcome is not.
    for (outcome, final_text) in [
        ("accepted", Some(text.as_str())),
        ("edited", Some("Thursday works, see you then!")),
        ("rejected", None),
    ] {
        let v: Value = h
            .call(
                method::DRAFT_FEEDBACK,
                json!({"stream_id": started.stream_id, "outcome": outcome,
                       "final_text": final_text, "insert_verified": true}),
            )
            .await;
        assert_eq!(v, Value::Null);
    }
    let v: Value = h
        .call(
            method::DRAFT_FEEDBACK,
            json!({"stream_id": "nope", "outcome": "rejected"}),
        )
        .await;
    assert_eq!(v, Value::Null);
    let e = h
        .call_err(
            method::DRAFT_FEEDBACK,
            json!({"stream_id": started.stream_id, "outcome": "loved"}),
        )
        .await;
    assert_eq!(e.code, code::INVALID_PARAMS);

    let fb = h.audit_eventually("draft.feedback", 4).await;
    let outcomes: Vec<&str> = fb.iter().filter_map(|d| d["outcome"].as_str()).collect();
    assert_eq!(outcomes, ["accepted", "edited", "rejected", "rejected"]);
    assert_eq!(fb[3]["known"], json!(false));
    assert_eq!(h.audit_eventually("draft.begin", 1).await.len(), 1);
    let end = h.audit_eventually("draft.end", 1).await;
    assert_eq!(end[0]["end"], json!("done"));
    assert!(end[0]["chars"].as_u64().unwrap() > 0);

    // No draft, thread or field text in the audit log.
    let all = format!("{:?}", h.audit());
    assert!(!all.contains(THREAD_SECRET));
    assert!(!all.contains("Thursday works"));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_stops_a_draft_mid_stream_and_shutdown_ends_one() {
    let mut h = start("", |_| {}).await;
    let mut notes = h.client.notifications();

    let started: DraftStarted = h
        .call(
            method::DRAFT_BEGIN,
            json!({"context": chat_context(), "instruction": SLOW}),
        )
        .await;
    let first = first_delta(&mut notes, &started.stream_id).await;
    assert!(first.contains("Sounds good"), "{first:?}");
    let v: Value = h
        .call(
            method::DRAFT_CANCEL,
            json!({"stream_id": started.stream_id}),
        )
        .await;
    assert_eq!(v, Value::Null);
    let (_, done) = collect(&mut notes, &started.stream_id).await;
    assert_eq!(done.error.as_deref(), Some("cancelled"));
    // Cancelling again (or an unknown stream) is fine.
    let _: Value = h
        .call(
            method::DRAFT_CANCEL,
            json!({"stream_id": started.stream_id}),
        )
        .await;
    let cancels = h.audit_eventually("draft.cancel", 2).await;
    assert_eq!(cancels[0]["running"], json!(true));
    assert_eq!(cancels[1]["running"], json!(false));
    let end = h.audit_eventually("draft.end", 1).await;
    assert_eq!(end[0]["end"], json!("cancelled"));

    // A draft in flight when the daemon stops is told so.
    let started: DraftStarted = h
        .call(
            method::DRAFT_BEGIN,
            json!({"context": chat_context(), "instruction": SLOW}),
        )
        .await;
    first_delta(&mut notes, &started.stream_id).await;
    let _ = h.stop.take().unwrap().send(());
    let (_, done) = collect(&mut notes, &started.stream_id).await;
    assert_eq!(done.error.as_deref(), Some("nuggetd is shutting down"));
    tokio::time::timeout(WAIT, h.server.take().unwrap())
        .await
        .expect("daemon stops promptly")
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secure_fields_excluded_apps_and_search_boxes_are_refused_before_anything_is_sent() {
    let mut h = start(r#"excluded_apps = ["com.example.bank"]"#, |_| {}).await;

    let mut secure = chat_context();
    secure["field"]["secure"] = json!(true);
    secure["field"]["value"] = json!(FIELD_SECRET);
    let e = h
        .call_err(method::DRAFT_BEGIN, json!({"context": secure}))
        .await;
    assert_eq!(e.code, code::INVALID_PARAMS);
    assert_eq!(e.data, Some(json!({"refused": "secure"})));

    // Recognised by its label even when the OS does not flag it.
    let mut labelled = chat_context();
    labelled["field"] = json!({"role": "text_field", "label": "Password", "value": FIELD_SECRET});
    let e = h
        .call_err(method::DRAFT_BEGIN, json!({"context": labelled}))
        .await;
    assert_eq!(e.data, Some(json!({"refused": "secure"})));

    // Built-in and configured exclusions both apply.
    for app in ["com.1password.1password", "com.example.bank"] {
        let mut excluded = chat_context();
        excluded["app"] = json!({"id": app, "name": "Vault"});
        let e = h
            .call_err(method::DRAFT_BEGIN, json!({"context": excluded}))
            .await;
        assert_eq!(e.code, code::INVALID_PARAMS);
        assert_eq!(e.data, Some(json!({"refused": "excluded_app"})), "{app}");
    }

    let mut search = chat_context();
    search["field"] = json!({"role": "search_field", "placeholder": "Search", "value": ""});
    let e = h
        .call_err(method::DRAFT_BEGIN, json!({"context": search}))
        .await;
    assert_eq!(e.code, code::UNSUPPORTED);
    assert_eq!(
        e.data,
        Some(json!({"refused": "not_draftable", "field_kind": "search"}))
    );

    // Prewarm says "would be refused" without touching the model.
    let v: Value = h.call("draft.prewarm", secure_hint()).await;
    assert_eq!(v, Value::Null);

    // The model saw nothing at all; the refusals are audited without the field's text.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(h.model.seen().is_empty(), "{:?}", h.model.seen());
    let refused = h.audit_eventually("draft.refused", 5).await;
    let reasons: Vec<&str> = refused
        .iter()
        .filter_map(|d| d["reason"].as_str())
        .collect();
    assert_eq!(
        reasons,
        [
            "secure",
            "secure",
            "excluded_app",
            "excluded_app",
            "not_draftable"
        ]
    );
    let all = format!("{:?}", h.audit());
    assert!(!all.contains(FIELD_SECRET));
    assert!(!all.contains(THREAD_SECRET));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminals_are_refused_unless_allowed() {
    let mut h = start("", |_| {}).await;
    let mut term = chat_context();
    term["app"] = json!({"id": "com.apple.Terminal", "name": "Terminal"});
    term["field"] = json!({"role": "text_area", "value": FIELD_SECRET});
    let e = h
        .call_err(method::DRAFT_BEGIN, json!({"context": term.clone()}))
        .await;
    assert_eq!(e.code, code::INVALID_PARAMS);
    assert_eq!(e.data, Some(json!({"refused": "terminal"})));
    let v: Value = h.call("draft.prewarm", term).await;
    assert_eq!(v, Value::Null);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(h.model.seen().is_empty(), "{:?}", h.model.seen());
    let refused = h.audit_eventually("draft.refused", 1).await;
    assert_eq!(refused[0]["reason"], json!("terminal"));
    assert!(!format!("{:?}", h.audit()).contains(FIELD_SECRET));
    h.stop().await;

    // Opted in: the terminal is drafted into like any other app.
    let mut h = start("allow_terminals = true", |_| {}).await;
    let mut notes = h.client.notifications();
    let mut term = chat_context();
    term["app"] = json!({"id": "com.apple.Terminal", "name": "Terminal"});
    let started: DraftStarted = h.call(method::DRAFT_BEGIN, json!({"context": term})).await;
    let (_, done) = collect(&mut notes, &started.stream_id).await;
    assert_eq!(done.error, None);
    h.stop().await;
}

fn secure_hint() -> Value {
    json!({"app": {"id": "com.apple.Safari", "name": "Safari"},
           "field": {"role": "text_field", "value": "", "secure": true}})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prewarm_names_the_provider_and_opens_its_connection() {
    let mut h = start("", |_| {}).await;
    let hint = json!({"app": {"id": "com.tinyspeck.slackmacgap", "name": "Slack"},
                      "field": {"role": "text_area", "value": ""}});
    let v: Value = h.call("draft.prewarm", hint).await;
    assert_eq!(v["provider"], json!("fake"));
    assert!(v["locality"].is_string());
    assert_eq!(v["retrieval"], json!(false));
    // The warm-up is a bodiless GET (no content is sent).
    let deadline = tokio::time::Instant::now() + WAIT;
    while h.model.seen().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let seen = h.model.seen();
    assert!(!seen.is_empty(), "prewarm reached the provider");
    assert!(seen.iter().all(|s| s.method == "GET"), "{seen:?}");
    assert!(seen[0].path.starts_with("/v1/"), "{seen:?}");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabled_drafting_refuses_but_still_reports_the_voice() {
    let mut h = start("enabled = false", |s| seed_chat(s, 0, 6)).await;
    let e = h
        .call_err(method::DRAFT_BEGIN, json!({"context": chat_context()}))
        .await;
    assert_eq!(e.code, code::UNSUPPORTED);
    assert_eq!(e.data, Some(json!({"refused": "disabled"})));
    let v: Value = h.call("draft.prewarm", chat_context()).await;
    assert_eq!(v, Value::Null);
    let s = voice_status_eventually(&h, |s| s.samples >= 6).await;
    assert_eq!(s.samples, 6);
    assert!(h.model.seen().is_empty());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_ahead_skips_cloud_providers_and_refused_fields_and_cancel_answers() {
    let mut h = start("", |_| {}).await;
    let audit_before = h.audit().len();
    // The only provider is a cloud one: nothing is sent ahead of the trigger.
    let v: Value = h
        .call(method::DRAFT_READ_AHEAD, json!({"context": chat_context()}))
        .await;
    assert_eq!(v, json!({"started": false, "skipped": "cloud"}));
    // A secure field is refused before anything else.
    let v: Value = h
        .call(method::DRAFT_READ_AHEAD, json!({"context": secure_hint()}))
        .await;
    assert_eq!(v, json!({"started": false, "skipped": "refused"}));
    // Nothing running: cancel answers false, with `{}` or without params.
    let v: Value = h.call(method::DRAFT_READ_AHEAD_CANCEL, json!({})).await;
    assert_eq!(v, json!({"cancelled": false}));
    let v: Value = h.call(method::DRAFT_READ_AHEAD_CANCEL, Value::Null).await;
    assert_eq!(v, json!({"cancelled": false}));
    // Bad params are an error, not a skip.
    let e = h
        .call_err(method::DRAFT_READ_AHEAD, json!({"nope": 1}))
        .await;
    assert_eq!(e.code, code::INVALID_PARAMS);
    // Nothing reached the model, and nothing was audited.
    assert!(h.model.seen().is_empty(), "{:?}", h.model.seen());
    assert_eq!(h.audit().len(), audit_before, "{:?}", h.audit());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_ahead_is_skipped_when_drafting_or_read_ahead_is_off() {
    for draft in ["enabled = false", "[draft.read_ahead]\nenabled = false"] {
        let mut h = start(draft, |_| {}).await;
        let v: Value = h
            .call(method::DRAFT_READ_AHEAD, json!({"context": chat_context()}))
            .await;
        assert_eq!(
            v,
            json!({"started": false, "skipped": "disabled"}),
            "{draft}"
        );
        let v: Value = h.call(method::DRAFT_READ_AHEAD_CANCEL, json!({})).await;
        assert_eq!(v, json!({"cancelled": false}));
        assert!(h.model.seen().is_empty());
        h.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn voice_profile_loads_at_startup_and_refreshes_after_an_ingest_commit() {
    let mut h = start("", |s| seed_chat(s, 0, 6)).await;
    let s = voice_status_eventually(&h, |s| s.samples >= 6).await;
    assert_eq!(s.samples, 6, "{s:?}");
    assert_eq!(s.channels, ["chat"]);
    assert!(s.last_built.is_some());

    // New messages land in the store; an ingest commit (a source added and scanned) makes the
    // profile pick them up.
    seed_chat(&h.store, 6, 9);
    let docs = h.tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let _: Value = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "files", "name": "Docs",
                   "settings": {"roots": [docs], "include": [], "exclude": []}}),
        )
        .await;
    let s = voice_status_eventually(&h, |s| s.samples >= 9).await;
    assert_eq!(s.samples, 9, "{s:?}");
    h.stop().await;
}
