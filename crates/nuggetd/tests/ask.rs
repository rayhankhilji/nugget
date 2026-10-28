//! `retrieve.ask` and `chat` item provenance through IPC: sensitive passages never reach a
//! cloud model (also not after a fallback from a local one), they do reach local models, and
//! the egress ledger lists the items that were sent.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use nugget_core::capability::Locality;
use nugget_core::egress::EgressEvent;
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::ipc::{AskStarted, ChatDelta, ChatDone, ChatStarted, method, notification};
use nugget_core::paths::Paths;
use nugget_ingest::MemorySecrets;
use nugget_ipc::{Client, Notification};
use nugget_store::{NewChunk, NewItem, Store, StoreKey, content_hash};
use nuggetd::{Daemon, DaemonOptions, Startup};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use wiremock::matchers::method as http_method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const WAIT: Duration = Duration::from_secs(20);
const SECRET: &str = "SECRETPASSAGE 4111 1111 1111 1111";

/// Streams a short answer for any chat request.
struct Answer;

impl Respond for Answer {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        let chunks = [
            json!({"choices": [{"index": 0, "delta": {"content": "Herons [1]"}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ];
        let mut body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        body.push_str("data: [DONE]\n\n");
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(body)
    }
}

struct Harness {
    _tmp: tempfile::TempDir,
    mock: MockServer,
    store: Store,
    client: Client,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<nugget_core::Result<()>>>,
}

/// `providers` is TOML for `[[providers]]`; `{mock}` is replaced by the mock's base URL.
async fn start(providers: &str) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path().to_path_buf());
    let mock = MockServer::start().await;
    Mock::given(http_method("POST"))
        .respond_with(Answer)
        .mount(&mock)
        .await;
    std::fs::create_dir_all(&paths.config).unwrap();
    let config = format!(
        "[runtime]\nenabled = false\n\n{}",
        providers.replace("{mock}", &format!("{}/v1", mock.uri()))
    );
    std::fs::write(paths.config_file(), config).unwrap();
    let mut opts = DaemonOptions::new(paths.clone());
    opts.store_key = Some(StoreKey::from_bytes([5; 32]));
    opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
    let daemon = match Daemon::start(opts).await.unwrap() {
        Startup::Ready(d) => d,
        Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
    };
    let store = daemon.store();
    seed(&store);
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(daemon.run_until(async {
        let _ = rx.await;
    }));
    let client = Client::connect(&paths).await.unwrap();
    Harness {
        _tmp: tmp,
        mock,
        store,
        client,
        stop: Some(tx),
        server: Some(server),
    }
}

fn iref(key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new("files-1"),
        key: ItemKey::new(key),
    }
}

/// One sensitive and one ordinary item about herons, as ingest would store them.
fn seed(store: &Store) {
    for (key, text, sensitive) in [
        ("bank.md", format!("Heron savings account {SECRET}."), true),
        (
            "walk.md",
            "We watched a heron fishing by the river.".to_owned(),
            false,
        ),
    ] {
        let item = NewItem::new("files-1", key, content_hash(&text))
            .with_title(key)
            .with_metadata(json!({"sensitive": sensitive}));
        store
            .upsert_item_with_chunks(&item, &[NewChunk::text(text)])
            .unwrap();
    }
}

impl Harness {
    async fn call<R: serde::de::DeserializeOwned>(&self, m: &str, params: Value) -> R {
        tokio::time::timeout(WAIT, self.client.call(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} failed: {e}"))
    }

    /// Bodies of every request the mock model received.
    async fn bodies(&self) -> String {
        self.mock
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .map(|r| String::from_utf8_lossy(&r.body).into_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn egress(&self, feature: &str) -> Vec<EgressEvent> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            self.store.flush_egress();
            let events: Vec<EgressEvent> =
                self.call(method::EGRESS_RECENT, json!({"limit": 50})).await;
            let found: Vec<EgressEvent> = events
                .into_iter()
                .filter(|e| e.feature == feature)
                .collect();
            if !found.is_empty() || tokio::time::Instant::now() > deadline {
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

const CLOUD: &str = r#"
[[providers]]
id = "remote"
kind = "openai_compatible"
base_url = "{mock}"
model = "remote-model"
tiers = ["synthesis"]
max_retries = 0
locality = "cloud"
"#;

/// A local provider nothing listens on, routed before the cloud one.
const DEAD_LOCAL_THEN_CLOUD: &str = r#"
[[providers]]
id = "dead"
kind = "openai_compatible"
base_url = "http://127.0.0.1:9/v1"
model = "dead-model"
tiers = ["synthesis"]
max_retries = 0
connect_timeout_secs = 2

[[providers]]
id = "remote"
kind = "openai_compatible"
base_url = "{mock}"
model = "remote-model"
tiers = ["synthesis"]
max_retries = 0
locality = "cloud"
"#;

const LOCAL: &str = r#"
[[providers]]
id = "ollama"
kind = "openai_compatible"
base_url = "{mock}"
model = "local-model"
tiers = ["synthesis"]
max_retries = 0
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_never_sends_sensitive_passages_to_a_cloud_model() {
    let mut h = start(DEAD_LOCAL_THEN_CLOUD).await;
    let mut notes = h.client.notifications();

    // Routed by tier: the local provider is tried first, fails, and the cloud one answers.
    // The context is packed again for the cloud provider, without the sensitive item.
    let started: AskStarted = h
        .call(
            method::RETRIEVE_ASK,
            json!({"query": "heron", "budget_tokens": 2000}),
        )
        .await;
    assert_eq!(started.provider.as_str(), "remote");
    assert_eq!(started.locality, Locality::Cloud);
    assert_eq!(started.excluded_sensitive, 1);
    let cited: Vec<&ItemRef> = started.citations.iter().map(|c| &c.item).collect();
    assert_eq!(cited, vec![&iref("walk.md")]);
    assert!(started.citations.iter().all(|c| !c.sensitive));
    let (text, done) = collect(&mut notes, &started.stream_id).await;
    assert_eq!(done.error, None);
    assert_eq!(text, "Herons [1]");

    let bodies = h.bodies().await;
    assert!(bodies.contains("watched a heron"), "context was sent");
    assert!(
        !bodies.contains("SECRETPASSAGE"),
        "a sensitive passage reached the cloud"
    );
    assert!(!bodies.contains("4111"));

    // The egress ledger names the items whose content left the device.
    let asks = h.egress("ask").await;
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert_eq!(asks[0].items, vec![iref("walk.md")]);

    // Plain `chat` forwards the client's item references to the ledger, too.
    let chat: ChatStarted = h
        .call(
            method::CHAT,
            json!({
                "provider": "remote",
                "tier": "synthesis",
                "messages": [{"role": "user", "text": "We watched a heron. Summarize."}],
                "items": [iref("walk.md")],
            }),
        )
        .await;
    let (_, done) = collect(&mut notes, &chat.stream_id).await;
    assert_eq!(done.error, None);
    let chats = h.egress("chat").await;
    assert_eq!(chats.len(), 1, "{chats:?}");
    assert_eq!(chats[0].items, vec![iref("walk.md")]);

    // An explicit cloud provider gets the same treatment.
    let started: AskStarted = h
        .call(
            method::RETRIEVE_ASK,
            json!({"query": "heron savings", "budget_tokens": 2000, "provider": "remote"}),
        )
        .await;
    assert_eq!(started.excluded_sensitive, 1);
    collect(&mut notes, &started.stream_id).await;
    assert!(!h.bodies().await.contains("SECRETPASSAGE"));

    // The audit log records the ask (without content).
    let audit = h.store.recent_audit(50).unwrap();
    let ask = audit
        .iter()
        .find(|r| r.action == "retrieve.ask")
        .expect("audited");
    assert_eq!(ask.detail["excluded_sensitive"], json!(1));
    assert!(!ask.detail.to_string().contains("SECRETPASSAGE"));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_gives_local_models_sensitive_passages() {
    let mut h = start(LOCAL).await;
    let mut notes = h.client.notifications();
    let started: AskStarted = h
        .call(
            method::RETRIEVE_ASK,
            json!({"query": "heron", "budget_tokens": 2000}),
        )
        .await;
    assert_eq!(started.provider.as_str(), "ollama");
    assert_eq!(started.locality, Locality::OnDevice);
    assert_eq!(started.excluded_sensitive, 0);
    assert_eq!(started.citations.len(), 2);
    let bank = started
        .citations
        .iter()
        .find(|c| c.item == iref("bank.md"))
        .expect("the sensitive item is cited");
    assert!(bank.sensitive);
    let (_, done) = collect(&mut notes, &started.stream_id).await;
    assert_eq!(done.error, None);
    assert!(h.bodies().await.contains("SECRETPASSAGE"));
    // Nothing left the device.
    h.store.flush_egress();
    let events: Vec<EgressEvent> = h.call(method::EGRESS_RECENT, json!({"limit": 50})).await;
    assert!(events.is_empty(), "{events:?}");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_rejects_bad_requests() {
    let mut h = start(CLOUD).await;
    for params in [
        json!({"query": "  ", "budget_tokens": 100}),
        json!({"query": "heron", "budget_tokens": 0}),
    ] {
        let r = h
            .client
            .call_value(method::RETRIEVE_ASK, params.clone())
            .await;
        assert!(r.is_err(), "{params}");
    }
    let r = h
        .client
        .call_value(
            method::RETRIEVE_ASK,
            json!({"query": "heron", "budget_tokens": 100, "provider": "nope"}),
        )
        .await;
    assert!(r.is_err());
    assert!(h.bodies().await.is_empty(), "nothing was sent");
    h.stop().await;
}
