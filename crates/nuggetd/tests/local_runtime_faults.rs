//! KK1 (QA GG6-3, GG6-5, GG6-17): the local model runtime never leaves a chat stuck. A hung
//! server fails the chat with a plain message within 15 s (and is restarted), a crash before
//! the first token is retried once, and a server that cannot start fails fast with one short
//! message, without a relaunch per request, until the user retries.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use nugget_core::capability::Tier;
use nugget_core::ipc::{
    ChatDelta, ChatDone, ChatStarted, ModelInfo, RuntimeInfo, RuntimeRole, RuntimeState, method,
    notification,
};
use nugget_core::paths::Paths;
use nugget_ingest::MemorySecrets;
use nugget_ipc::{Client, Notification};
use nugget_models::{GpuKind, Hardware, Manifest, ModelEntry, RuntimeAccel};
use nugget_runtime::{RuntimeOptions, Tuning};
use nugget_store::StoreKey;
use nuggetd::{Daemon, DaemonOptions, RuntimeOverrides, Startup};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use wiremock::MockServer;
use wiremock::matchers::path;
use wiremock::{Mock, ResponseTemplate};

const CALL: Duration = Duration::from_secs(30);
const EVENTUALLY: Duration = Duration::from_secs(45);
const REV: &str = "0123456789abcdef0123456789abcdef01234567";

struct Harness {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    client: Client,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<nugget_core::Result<()>>>,
}

impl Harness {
    async fn start(config: &str, runtime: RuntimeOverrides) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("nugget-faults-")
            .tempdir()
            .unwrap();
        let paths = Paths::rooted(tmp.path().join("home"));
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config_file(), config).unwrap();
        let mut opts = DaemonOptions::new(paths.clone());
        opts.store_key = Some(StoreKey::from_bytes([3; 32]));
        opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
        opts.runtime = runtime;
        let daemon = match Daemon::start(opts).await.unwrap() {
            Startup::Ready(d) => d,
            Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
        };
        let (tx, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(daemon.run_until(async {
            let _ = rx.await;
        }));
        let client = Client::connect(&paths).await.unwrap();
        Self {
            data: paths.data.clone(),
            _tmp: tmp,
            client,
            stop: Some(tx),
            server: Some(server),
        }
    }

    async fn call<R: serde::de::DeserializeOwned>(&self, m: &str, params: Value) -> R {
        tokio::time::timeout(CALL, self.client.call(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} failed: {e}"))
    }

    /// `models.install` the chat model and wait until it is installed (the call only starts
    /// the download).
    async fn install_chat_model(&self) {
        let _: Value = self
            .call(method::MODELS_INSTALL, json!({"id": "chat-model"}))
            .await;
        eventually("chat model installed", || async {
            let models: Vec<ModelInfo> = self.call(method::MODELS_LIST, json!({})).await;
            models.iter().any(|m| m.id == "chat-model" && m.installed)
        })
        .await;
    }

    /// Send a plain chat (no provider, synthesis tier) and wait for its answer.
    async fn chat(
        &self,
        watcher: &Arc<Mutex<Vec<Notification>>>,
    ) -> (Option<ChatStarted>, std::result::Result<String, String>) {
        // An error before the stream started comes back as the call's error.
        let params = json!({"provider": null, "tier": "synthesis",
                            "messages": [{"role": "user", "text": "hi"}]});
        let call = self.client.call::<_, ChatStarted>(method::CHAT, &params);
        let started = match tokio::time::timeout(CALL, call)
            .await
            .expect("chat timed out")
        {
            Ok(s) => s,
            Err(e) => return (None, Err(e.to_string())),
        };
        eventually("chat done", || async {
            watcher.lock().unwrap().iter().any(|n| {
                n.method == notification::CHAT_DONE
                    && n.params_as::<ChatDone>()
                        .is_ok_and(|d| d.stream_id == started.stream_id)
            })
        })
        .await;
        let done = watcher
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.method == notification::CHAT_DONE)
            .filter_map(|n| n.params_as::<ChatDone>().ok())
            .find(|d| d.stream_id == started.stream_id)
            .unwrap();
        if let Some(e) = done.error {
            return (Some(started), Err(e));
        }
        let text: String = watcher
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.method == notification::CHAT_DELTA)
            .filter_map(|n| n.params_as::<ChatDelta>().ok())
            .filter(|d| d.stream_id == started.stream_id)
            .map(|d| d.text)
            .collect();
        (Some(started), Ok(text))
    }

    async fn stop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(CALL, self.server.take().unwrap())
            .await
            .expect("daemon stops promptly")
            .unwrap()
            .unwrap();
    }
}

fn collect(client: &Client) -> Arc<Mutex<Vec<Notification>>> {
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut notes = client.notifications();
    let o = out.clone();
    tokio::spawn(async move {
        while let Some(n) = notes.next().await {
            o.lock().unwrap().push(n);
        }
    });
    out
}

async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + EVENTUALLY;
    while !f().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn fake_server() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nuggetd-fake-llama-server"))
}

/// A manifest with one tiny chat "model" served by `server`.
async fn chat_manifest(server: &MockServer) -> Manifest {
    let id = "chat-model";
    let bytes = format!("GGUF fake weights for {id}").into_bytes();
    let p = format!("/{id}/resolve/{REV}/{id}.gguf");
    Mock::given(path(p.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
        .mount(server)
        .await;
    Manifest {
        version: 1,
        models: vec![ModelEntry {
            id: id.into(),
            family: "test".into(),
            file_name: format!("{id}.gguf"),
            url: format!("{}{p}", server.uri()),
            mirrors: vec![],
            revision: REV.into(),
            size: bytes.len() as u64,
            sha256: nugget_store::content_hash(&bytes),
            min_ram_gb: 1,
            tiers: vec![Tier::Triage, Tier::Light, Tier::Synthesis],
            license: "Apache-2.0".into(),
            min_app_version: "0.0.1".into(),
        }],
        runtimes: vec![],
    }
}

impl Harness {
    async fn chat_info(&self) -> RuntimeInfo {
        let rt: Vec<RuntimeInfo> = self.call(method::RUNTIME_STATUS, json!({})).await;
        rt.into_iter()
            .find(|r| r.role == RuntimeRole::Chat)
            .unwrap()
    }

    fn launches(&self) -> usize {
        let log = self.data.join("logs").join("llama-server-chat.log");
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .matches("] starting chat-model")
            .count()
    }
}

/// The fake server with `args`, and the runtime's default watchdog and backoff timings (the
/// 15 s measure is about the shipped defaults).
async fn runtime(models: &MockServer, args: &[&str]) -> RuntimeOverrides {
    RuntimeOverrides {
        server_binary: Some((fake_server(), RuntimeAccel::Cpu)),
        options: Some(RuntimeOptions {
            health_timeout: Duration::from_secs(20),
            health_poll: Duration::from_millis(20),
            chat: Tuning {
                extra_args: args.iter().map(|s| s.to_string()).collect(),
                ..Tuning::default()
            },
            ..RuntimeOptions::default()
        }),
        manifest: Some(chat_manifest(models).await),
        hardware: Some(Hardware {
            total_ram: 16 << 30,
            available_ram: 8 << 30,
            cpu_cores: 4,
            gpu: GpuKind::Unknown,
        }),
    }
}

const RUNTIME_CONFIG: &str = "[runtime]\nchat_model = \"chat-model\"\n\
                              embedding_model = \"absent-embed\"\nidle_stop_minutes = 0\n";

/// A plain message: one short sentence or two, no transport dump.
fn assert_plain(msg: &str) {
    assert!(msg.len() < 300, "{msg}");
    for raw in [
        "http://",
        "error sending request",
        "SendRequest",
        "last output",
        " | ",
    ] {
        assert!(!msg.contains(raw), "raw detail {raw:?} in {msg}");
    }
}

/// GG6-3: a llama-server that accepts the request and never answers. The chat ends within
/// 15 s with "stopped responding", the status stops saying Ready, and the server comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hung_local_model_fails_the_chat_within_15_s_and_is_restarted() {
    let models = MockServer::start().await;
    let overrides = runtime(&models, &["--fake-hang-on-chat"]).await;
    let mut h = Harness::start(RUNTIME_CONFIG, overrides).await;
    let watcher = collect(&h.client);
    h.install_chat_model().await;
    eventually("chat model ready", || async {
        h.chat_info().await.state == RuntimeState::Ready
    })
    .await;

    let t = Instant::now();
    let (_, res) = h.chat(&watcher).await;
    let took = t.elapsed();
    let err = res.expect_err("a hung server cannot answer");
    assert!(took < Duration::from_secs(15), "the chat took {took:?}");
    assert!(err.contains("stopped responding"), "{err}");
    assert_plain(&err);
    // Restarted by the supervisor (the fake hangs only on a chat request).
    eventually("restarted", || async {
        h.chat_info().await.state == RuntimeState::Ready
    })
    .await;
    assert_eq!(h.launches(), 2);
    h.stop().await;
}

/// GG6-17: the server crashes before the first token; the chat is retried once on the
/// restarted server and answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crash_before_the_first_token_is_retried_once() {
    let models = MockServer::start().await;
    let marker = tempfile::tempdir().unwrap();
    let marker_path = marker.path().join("crashed");
    let overrides = runtime(
        &models,
        &["--fake-crash-on-chat-once", marker_path.to_str().unwrap()],
    )
    .await;
    let mut h = Harness::start(RUNTIME_CONFIG, overrides).await;
    let watcher = collect(&h.client);
    h.install_chat_model().await;
    eventually("chat model ready", || async {
        h.chat_info().await.state == RuntimeState::Ready
    })
    .await;
    let (started, res) = h.chat(&watcher).await;
    assert_eq!(res.as_deref(), Ok("Hello from fake"));
    assert_eq!(started.unwrap().provider.as_str(), "local-chat");
    assert!(marker_path.exists(), "the server did crash once");
    assert_eq!(h.launches(), 2);
    h.stop().await;
}

/// GG6-5: a server that cannot start. Every chat gets the same short message at once and no
/// new launch, until runtime.retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_that_cannot_start_fails_fast_until_retried() {
    let models = MockServer::start().await;
    let overrides = runtime(&models, &["--fake-exit-immediately", "1"]).await;
    let mut h = Harness::start(RUNTIME_CONFIG, overrides).await;
    let watcher = collect(&h.client);
    h.install_chat_model().await;
    eventually("start failed", || async {
        h.chat_info().await.state == RuntimeState::Failed
    })
    .await;
    let info = h.chat_info().await;
    let error = info.error.clone().unwrap();
    assert!(error.contains("couldn't start"), "{error}");
    assert_plain(&error);
    assert_eq!(
        info.detail.as_deref(),
        Some("fake: exiting immediately with 1")
    );
    let launched = h.launches();
    for _ in 0..5 {
        let t = Instant::now();
        let (_, res) = h.chat(&watcher).await;
        let err = res.expect_err("no model server");
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert_plain(&err);
        assert_eq!(err.matches("couldn't start").count(), 1, "{err}");
    }
    assert!(
        h.launches() <= launched + 1,
        "no relaunch per request: {} launches after {launched}",
        h.launches()
    );
    // Retry launches again at once.
    let before = h.launches();
    let r: Value = h.call("runtime.retry", json!({"role": "chat"})).await;
    assert_eq!(r["started"], true);
    eventually("retried", || async { h.launches() > before }).await;
    h.stop().await;
}
