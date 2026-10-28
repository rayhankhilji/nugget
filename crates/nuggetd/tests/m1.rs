//! M1 end-to-end: sources, ingest, search and context through IPC on a temporary home, with
//! in-memory source secrets and an explicit store key (no OS keychain); notifications to a
//! second client; and the local runtime against the fake `llama-server`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use nugget_connector::SecretStore;
use nugget_core::capability::{Locality, Tier};
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::SourceId;
use nugget_core::ipc::{
    ChatDelta, ChatDone, ChatStarted, ContextResponse, IngestProgress, IngestStatus, ProviderInfo,
    RuntimeInfo, RuntimeRole, RuntimeState, SearchHit, SourceInfo, SourceState, method,
    notification,
};
use nugget_core::paths::Paths;
use nugget_ingest::MemorySecrets;
use nugget_ipc::{Client, IpcError, Notification, code};
use nugget_models::{GpuKind, Hardware, Manifest, ModelEntry, ModelStore, RuntimeAccel};
use nugget_runtime::RuntimeOptions;
use nugget_store::{Store, StoreKey};
use nuggetd::{Daemon, DaemonOptions, RuntimeOverrides, Startup};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const CALL: Duration = Duration::from_secs(20);
/// Deadline for "eventually" conditions (ingest, runtime start).
const EVENTUALLY: Duration = Duration::from_secs(45);

struct Harness {
    tmp: tempfile::TempDir,
    paths: Paths,
    secrets: MemorySecrets,
    store: Store,
    client: Client,
    /// The daemon's local runtime, when enabled.
    runtime: Option<nugget_runtime::Runtime>,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<nugget_core::Result<()>>>,
}

fn temp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("nugget-m1-")
        .tempdir()
        .unwrap()
}

impl Harness {
    async fn start(config: &str) -> Self {
        Self::start_in(temp_home(), config, RuntimeOverrides::default()).await
    }

    async fn start_in(tmp: tempfile::TempDir, config: &str, runtime: RuntimeOverrides) -> Self {
        let paths = Paths::rooted(tmp.path().join("home"));
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config_file(), config).unwrap();
        let secrets = MemorySecrets::new();
        let mut opts = DaemonOptions::new(paths.clone());
        opts.store_key = Some(StoreKey::from_bytes([9; 32]));
        opts.source_secrets = Some(Arc::new(secrets.clone()));
        opts.runtime = runtime;
        let daemon = match Daemon::start(opts).await.unwrap() {
            Startup::Ready(d) => d,
            Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
        };
        let store = daemon.store();
        let local_runtime = daemon.runtime();
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
            runtime: local_runtime,
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

    async fn call_err(&self, m: &str, params: Value) -> (i64, String) {
        match self.client.call_value(m, params).await {
            Err(IpcError::Rpc(e)) => (e.code, e.message),
            other => panic!("{m}: expected an RPC error, got {other:?}"),
        }
    }

    async fn search(&self, q: &str) -> Vec<SearchHit> {
        self.call(method::SEARCH, json!({"query": q, "limit": 10}))
            .await
    }

    async fn stop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(CALL, self.server.take().unwrap())
            .await
            .expect("daemon stops promptly")
            .unwrap()
            .unwrap();
    }

    fn audit_actions(&self) -> Vec<(String, String, Value)> {
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
    loop {
        if f().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

const NO_RUNTIME: &str = "[runtime]\nenabled = false\n";

fn source_changes(notes: &Mutex<Vec<Notification>>) -> Vec<Value> {
    notes
        .lock()
        .unwrap()
        .iter()
        .filter(|n| n.method == notification::SOURCES_CHANGED)
        .map(|n| n.params.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_source_is_searchable_fresh_and_removable() {
    let mut h = Harness::start(NO_RUNTIME).await;
    // A second connection that never calls anything still gets the broadcasts.
    let watcher = Client::connect(&h.paths).await.unwrap();
    let seen = collect(&watcher);
    let caller_seen = collect(&h.client);

    let docs = h.tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let info: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            // `include: []` is what the desktop sends for "default types".
            json!({"kind": "files", "name": "Docs", "settings": {"roots": [docs], "include": [], "exclude": []}}),
        )
        .await;
    assert_eq!(info.id.as_str(), "files-1");
    assert_eq!(info.kind, "files");
    assert!(info.enabled);
    assert_eq!(info.settings, json!({"roots": [docs], "exclude": []}));

    // Create → searchable, with a file URL; freshness is reported.
    let file = docs.join("alpha.md");
    std::fs::write(&file, "# Alpha\n\nThe zebra ate a quartzite pebble.\n").unwrap();
    eventually("new file searchable", || async {
        !h.search("quartzite").await.is_empty()
    })
    .await;
    let hits = h.search("quartzite").await;
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].item.source.as_str(), "files-1");
    let url = hits[0].url.as_deref().expect("file url");
    assert!(
        url.starts_with("file://") && url.ends_with("alpha.md"),
        "{url}"
    );
    eventually("freshness reported", || async {
        let st: IngestStatus = h.call(method::INGEST_STATUS, json!({})).await;
        st.freshness_p50_ms.is_some() && st.freshness_p95_ms.is_some() && st.queue_depth == 0
    })
    .await;
    let st: IngestStatus = h.call(method::INGEST_STATUS, json!({})).await;
    assert_eq!(st.sources.len(), 1);
    assert_eq!(st.sources[0].items, 1);
    assert_eq!(st.embedding_provider, None, "no embedder configured");
    assert_eq!(st.triage_provider, None);

    // Edit → the new text replaces the old.
    std::fs::write(&file, "# Alpha\n\nNow it is about marmalade.\n").unwrap();
    eventually("edit reflected", || async {
        !h.search("marmalade").await.is_empty() && h.search("quartzite").await.is_empty()
    })
    .await;

    // retrieve.context packs it with a citation.
    let ctx: ContextResponse = h
        .call(
            method::RETRIEVE_CONTEXT,
            json!({"query": "marmalade", "budget_tokens": 300}),
        )
        .await;
    assert_eq!(ctx.citations.len(), 1, "{ctx:?}");
    assert_eq!(ctx.citations[0].n, 1);
    assert_eq!(ctx.citations[0].item.source.as_str(), "files-1");
    assert!(
        ctx.citations[0]
            .url
            .as_deref()
            .is_some_and(|u| u.ends_with("alpha.md"))
    );
    assert!(
        ctx.text.contains("[1]") && ctx.text.contains("marmalade"),
        "{}",
        ctx.text
    );
    let empty: ContextResponse = h
        .call(
            method::RETRIEVE_CONTEXT,
            json!({"query": "marmalade", "budget_tokens": 0}),
        )
        .await;
    assert!(empty.citations.is_empty() && empty.text.is_empty());

    // Delete → gone.
    std::fs::remove_file(&file).unwrap();
    eventually("deleted file leaves the index", || async {
        h.search("marmalade").await.is_empty()
    })
    .await;

    // Remove with purge clears everything the source indexed.
    std::fs::write(docs.join("beta.txt"), "kumquat orchard notes").unwrap();
    eventually("second file searchable", || async {
        !h.search("kumquat").await.is_empty()
    })
    .await;
    let _: Value = h
        .call(
            method::SOURCES_REMOVE,
            json!({"id": "files-1", "purge": true}),
        )
        .await;
    assert!(h.search("kumquat").await.is_empty());
    let list: Vec<SourceInfo> = h.call(method::SOURCES_LIST, json!({})).await;
    assert!(list.is_empty());
    let (c, _) = h
        .call_err(method::SOURCES_SYNC, json!({"id": "files-1"}))
        .await;
    assert_eq!(c, code::NOT_FOUND);

    // Both connections got the broadcasts: SourceInfo payloads, progress, and the removal.
    eventually("removal broadcast", || async {
        source_changes(&seen).contains(&json!({"id": "files-1", "removed": true}))
    })
    .await;
    for notes in [&seen, &caller_seen] {
        let changes = source_changes(notes);
        assert!(
            changes
                .iter()
                .any(|v| serde_json::from_value::<SourceInfo>(v.clone()).is_ok()),
            "{changes:?}"
        );
        let progress: Vec<IngestProgress> = notes
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.method == notification::INGEST_PROGRESS)
            .map(|n| n.params_as().unwrap())
            .collect();
        assert!(progress.iter().all(|p| p.source.as_str() == "files-1"));
        assert!(!progress.is_empty());
    }
    // No duplicates per connection: each SourceInfo change arrives once per emission.
    let removed = source_changes(&seen)
        .iter()
        .filter(|v| v.get("removed").is_some())
        .count();
    assert_eq!(removed, 1);

    let actions: Vec<String> = h.audit_actions().into_iter().map(|a| a.0).collect();
    assert!(actions.contains(&"source.add".to_string()), "{actions:?}");
    assert!(
        actions.contains(&"source.remove".to_string()),
        "{actions:?}"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secrets_are_stored_never_echoed_and_cleaned_up() {
    const PASSWORD: &str = "hunter2-very-secret";
    const CLIENT_SECRET: &str = "GOCSPX-client-secret-value";
    let mut h = Harness::start(NO_RUNTIME).await;
    let seen = collect(&h.client);

    // IMAP without a password parks in needs_auth (no network is touched).
    let closed_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let imap: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "imap", "name": "Mail", "settings": {
                "host": "127.0.0.1", "port": closed_port, "tls": "implicit",
                "username": "me@example.com", "folders": ["INBOX"]}}),
        )
        .await;
    eventually("imap needs auth", || async {
        let list: Vec<SourceInfo> = h.call(method::SOURCES_LIST, json!({})).await;
        list.iter()
            .any(|s| s.id == imap.id && s.state == SourceState::NeedsAuth)
    })
    .await;

    // Validation of secret.set.
    let (c, _) = h
        .call_err(
            method::SOURCES_SECRET_SET,
            json!({"id": imap.id, "secret": ""}),
        )
        .await;
    assert_eq!(c, code::INVALID_PARAMS);
    let (c, _) = h
        .call_err(
            method::SOURCES_SECRET_SET,
            json!({"id": "nope-1", "secret": "x"}),
        )
        .await;
    assert_eq!(c, code::NOT_FOUND);

    let r: Value = h
        .call(
            method::SOURCES_SECRET_SET,
            json!({"id": imap.id, "secret": PASSWORD}),
        )
        .await;
    assert_eq!(r, Value::Null);
    assert_eq!(
        h.secrets.get("password:imap-1").unwrap().as_deref(),
        Some(PASSWORD)
    );

    // OAuth kinds: the client secret goes to its own account, never into settings.
    let gmail: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "gmail", "name": "Gmail", "settings": {"client_id": "123-abc.apps.googleusercontent.com"}}),
        )
        .await;
    let (c, msg) = h
        .call_err(
            method::SOURCES_ADD,
            json!({"kind": "gcal", "name": "Cal", "settings": {"client_id": "x", "client_secret": CLIENT_SECRET}}),
        )
        .await;
    assert_eq!(c, code::INVALID_PARAMS, "{msg}");
    assert!(!msg.contains(CLIENT_SECRET));
    let _: Value = h
        .call(
            method::SOURCES_SECRET_SET,
            json!({"id": gmail.id, "secret": CLIENT_SECRET}),
        )
        .await;
    assert_eq!(
        h.secrets
            .get("oauth_client_secret:gmail-1")
            .unwrap()
            .as_deref(),
        Some(CLIENT_SECRET)
    );
    // Auth begins with a URL for the client to open (the daemon opens nothing).
    let auth: nugget_core::ipc::AuthBegin = h
        .call(method::SOURCES_AUTH_BEGIN, json!({"id": gmail.id}))
        .await;
    assert!(auth.url.starts_with("https://"), "{}", auth.url);
    assert!(auth.url.contains("123-abc.apps.googleusercontent.com"));
    assert!(!auth.url.contains(CLIENT_SECRET));

    // Files take no secret.
    let docs = h.tmp.path().join("plain");
    std::fs::create_dir_all(&docs).unwrap();
    let files: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "files", "name": "F", "settings": {"roots": [docs]}}),
        )
        .await;
    let (c, _) = h
        .call_err(
            method::SOURCES_SECRET_SET,
            json!({"id": files.id, "secret": "x"}),
        )
        .await;
    assert_eq!(c, code::INVALID_PARAMS);

    // No secret anywhere it could leak: responses, notifications, sources.toml, audit.
    let list: Value = h.call(method::SOURCES_LIST, json!({})).await;
    let status: Value = h.call(method::INGEST_STATUS, json!({})).await;
    let toml = std::fs::read_to_string(h.paths.config.join(nugget_ingest::SOURCES_FILE)).unwrap();
    let notes = serde_json::to_string(&*seen.lock().unwrap()).unwrap();
    let audit = serde_json::to_string(&h.audit_actions()).unwrap();
    for (what, text) in [
        ("sources.list", list.to_string()),
        ("ingest.status", status.to_string()),
        ("sources.toml", toml.clone()),
        ("notifications", notes),
        ("audit", audit.clone()),
    ] {
        assert!(!text.contains(PASSWORD), "{what} leaks the password");
        assert!(
            !text.contains(CLIENT_SECRET),
            "{what} leaks the client secret"
        );
    }
    assert!(
        toml.contains("oauth_client_secret:gmail-1"),
        "account name recorded"
    );
    assert!(audit.contains("\"account\":\"password:imap-1\""), "{audit}");
    assert!(audit.contains("source.auth.begin"), "{audit}");

    // Removing the sources deletes their keychain entries.
    for id in [&imap.id, &gmail.id] {
        let _: Value = h
            .call(method::SOURCES_REMOVE, json!({"id": id, "purge": true}))
            .await;
    }
    let left: Vec<String> = h.secrets.accounts();
    assert!(left.is_empty(), "{left:?}");
    h.stop().await;
}

// ---- local runtime ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_status_is_empty_when_disabled() {
    let mut h = Harness::start(NO_RUNTIME).await;
    let rt: Vec<RuntimeInfo> = h.call(method::RUNTIME_STATUS, json!({})).await;
    assert!(rt.is_empty());
    let providers: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    assert!(providers.is_empty());
    h.stop().await;
}

struct NoEgress;
impl EgressSink for NoEgress {
    fn record(&self, _event: EgressEvent) {}
}

fn sha256_hex(bytes: &[u8]) -> String {
    nugget_store::content_hash(bytes)
}

const REV: &str = "0123456789abcdef0123456789abcdef01234567";

/// A manifest with two tiny "models" served by a local HTTP server.
async fn test_manifest(server: &wiremock::MockServer) -> Manifest {
    let mut models = Vec::new();
    for (id, tiers) in [
        (
            "chat-model",
            vec![Tier::Triage, Tier::Light, Tier::Synthesis],
        ),
        ("embed-model", vec![Tier::Embedding]),
    ] {
        let bytes = format!("GGUF fake weights for {id}").into_bytes();
        let path = format!("/{id}/resolve/{REV}/{id}.gguf");
        wiremock::Mock::given(wiremock::matchers::path(path.clone()))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
            .mount(server)
            .await;
        models.push(ModelEntry {
            id: id.into(),
            family: "test".into(),
            file_name: format!("{id}.gguf"),
            url: format!("{}{path}", server.uri()),
            mirrors: vec![],
            revision: REV.into(),
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            min_ram_gb: 1,
            tiers,
            license: "Apache-2.0".into(),
            min_app_version: "0.0.1".into(),
        });
    }
    Manifest {
        version: 1,
        models,
        runtimes: vec![],
    }
}

fn fake_server() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nuggetd-fake-llama-server"))
}

fn fast_runtime() -> RuntimeOptions {
    RuntimeOptions {
        health_timeout: Duration::from_secs(15),
        health_poll: Duration::from_millis(20),
        watch_interval: Duration::from_millis(50),
        restart_backoff_base: Duration::from_millis(50),
        restart_backoff_max: Duration::from_millis(200),
        shutdown_grace: Duration::from_secs(2),
        ..RuntimeOptions::default()
    }
}

async fn runtime_ready(h: &Harness, role: RuntimeRole) -> bool {
    let rt: Vec<RuntimeInfo> = h.call(method::RUNTIME_STATUS, json!({})).await;
    rt.iter()
        .any(|r| r.role == role && r.state == RuntimeState::Ready)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_runtime_serves_embeddings_and_chat_and_starts_after_install() {
    let tmp = temp_home();
    let home = tmp.path().join("home");
    let models_server = wiremock::MockServer::start().await;
    let manifest = test_manifest(&models_server).await;

    // The embedding model is installed before start; the chat model is installed over IPC.
    let paths = Paths::rooted(home.clone());
    let store = ModelStore::open(&paths, Arc::new(NoEgress)).unwrap();
    store
        .install(manifest.get("embed-model").unwrap(), |_| {})
        .await
        .unwrap();
    drop(store);

    let overrides = RuntimeOverrides {
        server_binary: Some((fake_server(), RuntimeAccel::Cpu)),
        options: Some(fast_runtime()),
        manifest: Some(manifest),
        hardware: Some(Hardware {
            total_ram: 16 << 30,
            available_ram: 8 << 30,
            cpu_cores: 4,
            gpu: GpuKind::Unknown,
        }),
    };
    let config = "[runtime]\nchat_model = \"chat-model\"\nembedding_model = \"embed-model\"\nidle_stop_minutes = 0\n";
    let mut h = Harness::start_in(tmp, config, overrides).await;

    // Startup ensured the installed embedding model in the background.
    eventually("embedding runtime ready", || {
        runtime_ready(&h, RuntimeRole::Embedding)
    })
    .await;
    let rt: Vec<RuntimeInfo> = h.call(method::RUNTIME_STATUS, json!({})).await;
    let chat = rt.iter().find(|r| r.role == RuntimeRole::Chat).unwrap();
    assert_eq!(chat.state, RuntimeState::NotInstalled);
    assert_eq!(chat.provider.as_str(), "local-chat");
    eventually("embedding provider routed", || async {
        let st: IngestStatus = h.call(method::INGEST_STATUS, json!({})).await;
        st.embedding_provider.as_ref().map(|p| p.as_str()) == Some("local-embedding")
    })
    .await;
    let providers: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    assert_eq!(
        providers.len(),
        1,
        "only installed runtimes are listed: {providers:?}"
    );
    assert_eq!(providers[0].id.as_str(), "local-embedding");
    assert_eq!(providers[0].locality, Locality::OnDevice);
    assert_eq!(providers[0].tiers, vec![Tier::Embedding]);
    assert!(providers[0].capabilities.as_ref().unwrap().embeddings);

    // Indexed text is embedded by the local server: the store gains vectors.
    let docs = h.tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("n.md"), "Saffron risotto needs patience.").unwrap();
    let _: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "files", "name": "Docs", "settings": {"roots": [docs]}}),
        )
        .await;
    eventually("embedded file searchable", || async {
        !h.search("saffron").await.is_empty()
    })
    .await;
    assert_eq!(h.store.embedding_dim().unwrap(), Some(8));
    // Hybrid search still finds it.
    let hits = h.search("saffron risotto").await;
    assert!(!hits.is_empty());

    // Installing the chat model starts its server and routes chat tiers to it.
    let watcher = collect(&h.client);
    let _: Value = h
        .call(method::MODELS_INSTALL, json!({"id": "chat-model"}))
        .await;
    eventually("chat runtime ready after install", || {
        runtime_ready(&h, RuntimeRole::Chat)
    })
    .await;
    eventually("triage routed locally", || async {
        let st: IngestStatus = h.call(method::INGEST_STATUS, json!({})).await;
        st.triage_provider.as_ref().map(|p| p.as_str()) == Some("local-chat")
    })
    .await;
    let started: ChatStarted = h
        .call(
            method::CHAT,
            json!({"provider": null, "tier": "light", "messages": [{"role": "user", "text": "hi"}]}),
        )
        .await;
    assert_eq!(started.provider.as_str(), "local-chat");
    eventually("chat done", || async {
        watcher.lock().unwrap().iter().any(|n| {
            n.method == notification::CHAT_DONE
                && n.params_as::<ChatDone>()
                    .is_ok_and(|d| d.stream_id == started.stream_id && d.error.is_none())
        })
    })
    .await;
    let text: String = watcher
        .lock()
        .unwrap()
        .iter()
        .filter(|n| n.method == notification::CHAT_DELTA)
        .filter_map(|n| n.params_as::<ChatDelta>().ok())
        .filter(|d| d.stream_id == started.stream_id)
        .map(|d| d.text)
        .collect();
    assert!(!text.is_empty());

    // Shutdown stops the servers.
    h.stop().await;
    assert!(pid_files_gone(&home.join("data").join("runtime")));
}

/// `models.install` of an update for a model whose server is running restarts the server on
/// the new weights (the runtime compares the installed model's SHA-256).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn installing_a_model_update_restarts_its_running_server() {
    let tmp = temp_home();
    let home = tmp.path().join("home");
    let models_server = wiremock::MockServer::start().await;
    let manifest = test_manifest(&models_server).await;

    // An older copy of the embedding model is installed: same id, other weights.
    let mut old = manifest.get("embed-model").unwrap().clone();
    let old_bytes = b"GGUF older fake weights for embed-model".to_vec();
    let old_path = format!("/embed-model/resolve/{REV}/old.gguf");
    wiremock::Mock::given(wiremock::matchers::path(old_path.clone()))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(old_bytes.clone()))
        .mount(&models_server)
        .await;
    old.url = format!("{}{old_path}", models_server.uri());
    old.size = old_bytes.len() as u64;
    old.sha256 = sha256_hex(&old_bytes);
    let paths = Paths::rooted(home.clone());
    let store = ModelStore::open(&paths, Arc::new(NoEgress)).unwrap();
    store.install(&old, |_| {}).await.unwrap();
    drop(store);

    let overrides = RuntimeOverrides {
        server_binary: Some((fake_server(), RuntimeAccel::Cpu)),
        options: Some(fast_runtime()),
        manifest: Some(manifest),
        hardware: Some(Hardware {
            total_ram: 16 << 30,
            available_ram: 8 << 30,
            cpu_cores: 4,
            gpu: GpuKind::Unknown,
        }),
    };
    let config = "[runtime]\nchat_model = \"chat-model\"\nembedding_model = \"embed-model\"\nidle_stop_minutes = 0\n";
    let mut h = Harness::start_in(tmp, config, overrides).await;
    let runtime = h.runtime.clone().expect("runtime enabled");
    eventually("embedding runtime ready", || {
        runtime_ready(&h, RuntimeRole::Embedding)
    })
    .await;
    let before = runtime.details(RuntimeRole::Embedding).unwrap();
    let models: Vec<nugget_core::ipc::ModelInfo> = h.call(method::MODELS_LIST, json!({})).await;
    assert!(
        models
            .iter()
            .any(|m| m.id == "embed-model" && m.update_available),
        "{models:?}"
    );

    let _: Value = h
        .call(method::MODELS_INSTALL, json!({"id": "embed-model"}))
        .await;
    eventually("server restarted on the new weights", || {
        let runtime = runtime.clone();
        let before = before.clone();
        async move {
            runtime
                .details(RuntimeRole::Embedding)
                .is_some_and(|d| d.info.state == RuntimeState::Ready && d.pid != before.pid)
        }
    })
    .await;
    let models: Vec<nugget_core::ipc::ModelInfo> = h.call(method::MODELS_LIST, json!({})).await;
    assert!(
        models
            .iter()
            .any(|m| m.id == "embed-model" && m.installed && !m.update_available)
    );
    h.stop().await;
}

/// Files indexed while no embedder is installed are text-only; installing the embedding model
/// starts its server, whose readiness triggers the ingest re-embed job.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_indexed_before_the_embedder_is_installed_get_vectors_once_it_is_ready() {
    let tmp = temp_home();
    let models_server = wiremock::MockServer::start().await;
    let manifest = test_manifest(&models_server).await;
    let overrides = RuntimeOverrides {
        server_binary: Some((fake_server(), RuntimeAccel::Cpu)),
        options: Some(fast_runtime()),
        manifest: Some(manifest),
        hardware: Some(Hardware {
            total_ram: 16 << 30,
            available_ram: 8 << 30,
            cpu_cores: 4,
            gpu: GpuKind::Unknown,
        }),
    };
    let config = "[runtime]\nchat_model = \"chat-model\"\nembedding_model = \"embed-model\"\nidle_stop_minutes = 0\n";
    let mut h = Harness::start_in(tmp, config, overrides).await;

    let docs = h.tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("n.md"), "Saffron risotto needs patience.").unwrap();
    let _: SourceInfo = h
        .call(
            method::SOURCES_ADD,
            json!({"kind": "files", "name": "Docs", "settings": {"roots": [docs]}}),
        )
        .await;
    eventually("text-only file searchable", || async {
        !h.search("saffron").await.is_empty()
    })
    .await;
    assert_eq!(h.store.embedding_dim().unwrap(), None);
    assert_eq!(h.store.items_missing_embeddings(None, 10).unwrap().len(), 1);

    let _: Value = h
        .call(method::MODELS_INSTALL, json!({"id": "embed-model"}))
        .await;
    eventually("the indexed file gained vectors", || async {
        h.store
            .items_missing_embeddings(None, 10)
            .unwrap()
            .is_empty()
    })
    .await;
    assert_eq!(h.store.embedding_dim().unwrap(), Some(8));
    assert!(
        !h.store.search_vector(&[0.5; 8], 5).unwrap().is_empty(),
        "reachable by vector search"
    );
    h.stop().await;
}

fn pid_files_gone(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |d| {
        d.filter_map(|e| e.ok())
            .all(|e| e.path().extension().is_none_or(|x| x != "pid"))
    })
}

#[test]
fn source_ids_are_plain() {
    // Guard for the account naming the daemon relies on (`oauth_client_secret:<id>`).
    assert!(nugget_ingest::account_belongs_to(
        "oauth_client_secret:gmail-1",
        &SourceId::new("gmail-1")
    ));
}
