//! The first chat right after the local chat model was installed, while its server is still
//! loading: it waits for the server instead of failing with "no provider for tier ... has the
//! required capabilities". And when that start fails, chat still falls back to the next
//! provider.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use wiremock::matchers::{method as http_method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CALL: Duration = Duration::from_secs(30);
const EVENTUALLY: Duration = Duration::from_secs(45);
const REV: &str = "0123456789abcdef0123456789abcdef01234567";
/// How long the fake server reports "loading": far longer than the IPC round trips between
/// seeing the server start and the chat request, so the chat is sent before it is ready.
const LOAD_MS: u64 = 5000;

struct Harness {
    _tmp: tempfile::TempDir,
    client: Client,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<nugget_core::Result<()>>>,
}

impl Harness {
    async fn start(config: &str, runtime: RuntimeOverrides) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("nugget-lcs-")
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

    async fn chat_state(&self) -> Option<RuntimeState> {
        let rt: Vec<RuntimeInfo> = self.call(method::RUNTIME_STATUS, json!({})).await;
        rt.iter()
            .find(|r| r.role == RuntimeRole::Chat)
            .map(|r| r.state)
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
    async fn chat(&self, watcher: &Arc<Mutex<Vec<Notification>>>) -> (ChatStarted, String) {
        let started: ChatStarted = self
            .call(
                method::CHAT,
                json!({"provider": null, "tier": "synthesis",
                       "messages": [{"role": "user", "text": "hi"}]}),
            )
            .await;
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
        assert!(done.error.is_none(), "chat failed: {:?}", done.error);
        let text: String = watcher
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.method == notification::CHAT_DELTA)
            .filter_map(|n| n.params_as::<ChatDelta>().ok())
            .filter(|d| d.stream_id == started.stream_id)
            .map(|d| d.text)
            .collect();
        (started, text)
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

/// The fake server, loading for `LOAD_MS` before it reports healthy.
async fn slow_runtime(models: &MockServer, health_timeout: Duration) -> RuntimeOverrides {
    RuntimeOverrides {
        server_binary: Some((fake_server(), RuntimeAccel::Cpu)),
        options: Some(RuntimeOptions {
            health_timeout,
            health_poll: Duration::from_millis(20),
            watch_interval: Duration::from_millis(50),
            restart_backoff_base: Duration::from_millis(50),
            restart_backoff_max: Duration::from_millis(200),
            max_restarts: 0,
            shutdown_grace: Duration::from_secs(2),
            chat: Tuning {
                extra_args: vec!["--fake-start-delay-ms".into(), LOAD_MS.to_string()],
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

/// QA: the first chat right after onboarding installed the local models failed with "no
/// provider for tier Synthesis has the required capabilities" because the chat slot was
/// routable for nothing until its server first became ready.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chat_sent_while_the_new_chat_model_loads_waits_for_its_server() {
    let models = MockServer::start().await;
    let overrides = slow_runtime(&models, Duration::from_secs(30)).await;
    let mut h = Harness::start(RUNTIME_CONFIG, overrides).await;
    assert_eq!(h.chat_state().await, Some(RuntimeState::NotInstalled));
    let watcher = collect(&h.client);

    h.install_chat_model().await;
    // The install finished and the daemon is starting the server, which loads for `LOAD_MS`.
    eventually("chat server starting", || async {
        h.chat_state().await != Some(RuntimeState::NotInstalled)
            && h.chat_state().await != Some(RuntimeState::Stopped)
    })
    .await;
    assert_eq!(
        h.chat_state().await,
        Some(RuntimeState::Starting),
        "the chat server should still be loading"
    );

    let (started, text) = h.chat(&watcher).await;
    assert_eq!(started.provider.as_str(), "local-chat");
    assert_eq!(text, "Hello from fake");
    assert_eq!(h.chat_state().await, Some(RuntimeState::Ready));
    h.stop().await;
}

/// The local chat model is installed but its server never becomes ready (health timeout): a
/// chat sent meanwhile still gets its answer from the configured cloud provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chat_falls_back_when_the_new_chat_models_server_fails_to_start() {
    let models = MockServer::start().await;
    let remote = MockServer::start().await;
    let chunks = [
        json!({"choices": [{"index": 0, "delta": {"content": "Hello from remote"}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
    ];
    let mut body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    Mock::given(http_method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&remote)
        .await;
    // Loading takes longer than the health timeout: the start fails.
    let overrides = slow_runtime(&models, Duration::from_millis(1500)).await;
    let config = format!(
        "{RUNTIME_CONFIG}\n[[providers]]\nid = \"remote\"\nkind = \"openai_compatible\"\n\
         base_url = \"{}/v1\"\nmodel = \"remote-model\"\ntiers = [\"synthesis\"]\n\
         max_retries = 0\nlocality = \"cloud\"\n",
        remote.uri()
    );
    let mut h = Harness::start(&config, overrides).await;
    let watcher = collect(&h.client);
    h.install_chat_model().await;
    eventually("chat server starting", || async {
        matches!(
            h.chat_state().await,
            Some(RuntimeState::Starting | RuntimeState::Failed)
        )
    })
    .await;

    let (started, text) = h.chat(&watcher).await;
    assert_eq!(started.provider.as_str(), "remote");
    assert_eq!(text, "Hello from remote");
    eventually("the local start gave up", || async {
        matches!(h.chat_state().await, Some(RuntimeState::Failed))
    })
    .await;
    // After the failed start chat keeps working.
    let (started, _) = h.chat(&watcher).await;
    assert_eq!(started.provider.as_str(), "remote");
    h.stop().await;
}
