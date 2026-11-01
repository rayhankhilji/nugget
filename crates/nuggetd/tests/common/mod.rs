//! The in-process daemon harness every end-to-end test shares.
#![allow(dead_code)]

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use nugget_core::capability::Tier;
use nugget_core::ipc::{
    ChatDelta, ChatDone, ChatStarted, ModelInfo, RuntimeInfo, RuntimeRole, method, notification,
};
use nugget_core::paths::Paths;
use nugget_ingest::MemorySecrets;
use nugget_ipc::{Client, IpcError, Notification};
use nugget_models::{GpuKind, Hardware, Manifest, ModelEntry, RuntimeAccel};
use nugget_runtime::{RuntimeOptions, Tuning};
use nugget_store::{Store, StoreKey};
use nuggetd::{Daemon, DaemonOptions, RuntimeOverrides, Startup};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const CALL: Duration = Duration::from_secs(30);
pub const EVENTUALLY: Duration = Duration::from_secs(45);
pub const NO_RUNTIME: &str = "[runtime]\nenabled = false\n";
pub const KEY: [u8; 32] = [7; 32];

pub struct Harness {
    pub tmp: tempfile::TempDir,
    pub paths: Paths,
    pub secrets: MemorySecrets,
    pub store: Store,
    pub client: Client,
    pub runtime: Option<nugget_runtime::Runtime>,
    pub vault: Option<PathBuf>,
    pub mcp_addr: Option<SocketAddr>,
    pub stop: Option<oneshot::Sender<()>>,
    pub server: Option<JoinHandle<nugget_core::Result<()>>>,
}

impl Harness {
    /// A daemon on a fresh home with `config`, store key [`KEY`] and in-memory secrets.
    pub async fn start(config: &str) -> Self {
        Self::start_with(config, |_| {}).await
    }

    pub async fn start_with(config: &str, tweak: impl FnOnce(&mut DaemonOptions)) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        Self::start_in(tmp, config, tweak).await
    }

    /// Like [`Harness::start_with`] on a given temporary root (the home is `<tmp>/home`).
    pub async fn start_in(
        tmp: tempfile::TempDir,
        config: &str,
        tweak: impl FnOnce(&mut DaemonOptions),
    ) -> Self {
        let paths = Paths::rooted(tmp.path().join("home"));
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config_file(), config).unwrap();
        let secrets = MemorySecrets::new();
        let mut opts = DaemonOptions::new(paths.clone());
        opts.store_key = Some(StoreKey::from_bytes(KEY));
        opts.source_secrets = Some(Arc::new(secrets.clone()));
        tweak(&mut opts);
        let daemon = match Daemon::start(opts).await.unwrap() {
            Startup::Ready(d) => d,
            Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
        };
        let (store, runtime) = (daemon.store(), daemon.runtime());
        let (vault, mcp_addr) = (daemon.vault_root(), daemon.mcp_http_addr());
        let (tx, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(daemon.run_until(async {
            let _ = rx.await;
        }));
        let client = Client::connect(&paths).await.unwrap();
        Self {
            tmp,
            paths,
            secrets,
            store,
            client,
            runtime,
            vault,
            mcp_addr,
            stop: Some(tx),
            server: Some(server),
        }
    }

    /// Stops the daemon and starts a new one on the same home and config.
    pub async fn restart(mut self, tweak: impl FnOnce(&mut DaemonOptions)) -> Self {
        self.stop().await;
        let config = std::fs::read_to_string(self.paths.config_file()).unwrap();
        Self::start_in(self.tmp, &config, tweak).await
    }

    pub async fn call<R: serde::de::DeserializeOwned>(&self, m: &str, params: Value) -> R {
        tokio::time::timeout(CALL, self.client.call(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} failed: {e}"))
    }

    pub async fn try_call(&self, m: &str, params: Value) -> Result<Value, IpcError> {
        tokio::time::timeout(CALL, self.client.call_value(m, params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
    }

    /// The code and message of the RPC error `m` must fail with.
    pub async fn call_err(&self, m: &str, params: Value) -> (i64, String) {
        match self.try_call(m, params).await {
            Err(IpcError::Rpc(e)) => (e.code, e.message),
            other => panic!("{m}: expected an RPC error, got {other:?}"),
        }
    }

    /// Stops the daemon and waits for it to exit cleanly.
    pub async fn stop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(CALL, self.server.take().unwrap())
            .await
            .expect("daemon stops promptly")
            .unwrap()
            .unwrap();
    }

    /// The audit log, oldest first, as (action, target, detail).
    pub fn audit(&self) -> Vec<(String, String, Value)> {
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
}

/// Every notification a client receives, in order.
pub fn collect(client: &Client) -> Arc<Mutex<Vec<Notification>>> {
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

/// Polls `f` until it holds, failing after [`EVENTUALLY`].
pub async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + EVENTUALLY;
    while !f().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The fake `llama-server` built for these tests.
pub fn fake_server() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nuggetd-fake-llama-server"))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    nugget_store::content_hash(bytes)
}

impl Harness {
    pub async fn search(&self, q: &str) -> Vec<nugget_core::ipc::SearchHit> {
        self.call(method::SEARCH, json!({"query": q, "limit": 10}))
            .await
    }

    /// Adds a files source over `<tmp>/<dir>` holding `files` (name, text).
    pub async fn add_files<S: AsRef<str>>(
        &self,
        dir: &str,
        files: &[(&str, S)],
    ) -> (PathBuf, nugget_core::ipc::SourceInfo) {
        let docs = self.tmp.path().join(dir);
        std::fs::create_dir_all(&docs).unwrap();
        for (name, text) in files {
            std::fs::write(docs.join(name), text.as_ref()).unwrap();
        }
        let info = self
            .call(
                method::SOURCES_ADD,
                json!({"kind": "files", "name": dir, "settings": {"roots": [docs]}}),
            )
            .await;
        (docs, info)
    }

    pub async fn runtime_state(&self, role: RuntimeRole) -> Option<RuntimeInfo> {
        let rt: Vec<RuntimeInfo> = self.call(method::RUNTIME_STATUS, json!({})).await;
        rt.into_iter().find(|r| r.role == role)
    }

    /// `models.install` and wait until it is installed (the call only starts the download).
    pub async fn install(&self, id: &str) {
        let _: Value = self.call(method::MODELS_INSTALL, json!({"id": id})).await;
        eventually("model installed", || async {
            let models: Vec<ModelInfo> = self.call(method::MODELS_LIST, json!({})).await;
            models.iter().any(|m| m.id == id && m.installed)
        })
        .await;
    }

    /// Sends `chat` params and waits for the stream's end: the start (or the call's error)
    /// and the streamed text (or the stream's error).
    pub async fn chat(
        &self,
        watcher: &Arc<Mutex<Vec<Notification>>>,
        params: Value,
    ) -> (Option<ChatStarted>, Result<String, String>) {
        let call = self.client.call::<_, ChatStarted>(method::CHAT, &params);
        let started = match tokio::time::timeout(CALL, call)
            .await
            .expect("chat timed out")
        {
            Ok(s) => s,
            Err(e) => return (None, Err(e.to_string())),
        };
        let id = started.stream_id.clone();
        let done = || {
            watcher
                .lock()
                .unwrap()
                .iter()
                .filter(|n| n.method == notification::CHAT_DONE)
                .filter_map(|n| n.params_as::<ChatDone>().ok())
                .find(|d| d.stream_id == id)
        };
        eventually("chat done", || async { done().is_some() }).await;
        if let Some(e) = done().unwrap().error {
            return (Some(started), Err(e));
        }
        let text = watcher
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.method == notification::CHAT_DELTA)
            .filter_map(|n| n.params_as::<ChatDelta>().ok())
            .filter(|d| d.stream_id == id)
            .map(|d| d.text)
            .collect();
        (Some(started), Ok(text))
    }
}

/// A plain user chat with no provider named.
pub fn chat_params(tier: &str, text: &str) -> Value {
    json!({"provider": null, "tier": tier, "messages": [{"role": "user", "text": text}]})
}

pub const REV: &str = "0123456789abcdef0123456789abcdef01234567";

/// A manifest of tiny fake "models" served by `server`.
pub async fn manifest(server: &MockServer, models: &[(&str, &[Tier])]) -> Manifest {
    let mut out = Vec::new();
    for (id, tiers) in models {
        let bytes = format!("GGUF fake weights for {id}").into_bytes();
        out.push(model_entry(server, id, &bytes, "", tiers).await);
    }
    Manifest {
        version: 1,
        models: out,
        runtimes: vec![],
    }
}

/// One manifest entry for `bytes`, served by `server` (`tag` varies the URL).
pub async fn model_entry(
    server: &MockServer,
    id: &str,
    bytes: &[u8],
    tag: &str,
    tiers: &[Tier],
) -> ModelEntry {
    let p = format!("/{id}/resolve/{REV}/{tag}{id}.gguf");
    Mock::given(path(p.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .mount(server)
        .await;
    ModelEntry {
        id: id.into(),
        family: "test".into(),
        file_name: format!("{id}.gguf"),
        url: format!("{}{p}", server.uri()),
        mirrors: vec![],
        revision: REV.into(),
        size: bytes.len() as u64,
        sha256: sha256_hex(bytes),
        min_ram_gb: 1,
        tiers: tiers.to_vec(),
        license: "Apache-2.0".into(),
        min_app_version: "0.0.1".into(),
    }
}

/// Supervisor timings fast enough for tests, with `extra_args` for the fake chat server.
pub fn fast_runtime(extra_args: &[&str]) -> RuntimeOptions {
    RuntimeOptions {
        health_timeout: Duration::from_secs(15),
        health_poll: Duration::from_millis(20),
        watch_interval: Duration::from_millis(50),
        restart_backoff_base: Duration::from_millis(50),
        restart_backoff_max: Duration::from_millis(200),
        shutdown_grace: Duration::from_secs(2),
        chat: Tuning {
            extra_args: extra_args.iter().map(|s| s.to_string()).collect(),
            ..Tuning::default()
        },
        ..RuntimeOptions::default()
    }
}

/// The fake `llama-server` with `options`, `manifest` and a 16 GB machine.
pub fn overrides(manifest: Manifest, options: RuntimeOptions) -> RuntimeOverrides {
    RuntimeOverrides {
        server_binary: Some((fake_server(), RuntimeAccel::Cpu)),
        options: Some(options),
        manifest: Some(manifest),
        hardware: Some(Hardware {
            total_ram: 16 << 30,
            available_ram: 8 << 30,
            cpu_cores: 4,
            gpu: GpuKind::Unknown,
        }),
    }
}

/// An OpenAI-compatible streaming body of `chunks`.
pub fn sse(chunks: &[Value]) -> String {
    let mut body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    body
}

/// An OpenAI-compatible streaming answer of `text`.
pub fn sse_text(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse(&[
            json!({"choices": [{"index": 0, "delta": {"content": text}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ]))
}

pub const EXE: &str = env!("CARGO_BIN_EXE_nuggetd");

/// Options for MCP tests: no power policy, and setup snippets that launch this test's
/// `nuggetd` on this home.
pub fn mcp_opts(o: &mut DaemonOptions) {
    o.power = Some(Arc::new(nugget_ingest::NoPowerPolicy));
    o.home = Some(o.paths.data.parent().unwrap().to_path_buf());
    o.nuggetd_exe = Some(PathBuf::from(EXE));
}

pub type McpClient = rmcp::service::RunningService<rmcp::RoleClient, ()>;

pub async fn http_client(url: &str, token: &str) -> Result<McpClient, String> {
    use rmcp::ServiceExt;
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token.to_owned()),
    );
    ().serve(transport).await.map_err(|e| e.to_string())
}

/// Calls MCP tool `name` and returns its text and structured content.
pub async fn tool(client: &McpClient, name: &str, args: Value) -> String {
    let params = rmcp::model::CallToolRequestParams::new(name.to_owned())
        .with_arguments(args.as_object().unwrap().clone());
    let res = client
        .call_tool(params)
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut out: String = res
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect();
    if let Some(s) = &res.structured_content {
        out.push_str(&s.to_string());
    }
    out
}

/// Scopes of a client that sees every source but nothing sensitive.
pub fn all_scopes() -> Value {
    json!({"sources": null, "tools": [], "include_sensitive": false})
}
