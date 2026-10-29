//! Search re-ranking through the daemon: an external `[retrieve.reranker]`, and the local
//! runtime's `rerank` role (`[runtime] rerank_model`) against the fake `llama-server`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nugget_core::capability::Tier;
use nugget_core::ipc::{RuntimeInfo, RuntimeRole, RuntimeState, SearchHit, SourceInfo, method};
use nugget_core::paths::Paths;
use nugget_infer::SecretResolver;
use nugget_ingest::MemorySecrets;
use nugget_ipc::Client;
use nugget_models::{GpuKind, Hardware, Manifest, ModelEntry, RuntimeAccel};
use nugget_runtime::RuntimeOptions;
use nugget_store::StoreKey;
use nuggetd::{Daemon, DaemonOptions, RuntimeOverrides, Startup};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use wiremock::matchers::{method as http_method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CALL: Duration = Duration::from_secs(20);
const EVENTUALLY: Duration = Duration::from_secs(45);

struct Harness {
    tmp: tempfile::TempDir,
    client: Client,
    runtime: Option<nugget_runtime::Runtime>,
    stop: Option<oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<nugget_core::Result<()>>>,
}

impl Harness {
    async fn start(config: &str, runtime: RuntimeOverrides) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("nugget-rr-")
            .tempdir()
            .unwrap();
        let paths = Paths::rooted(tmp.path().join("home"));
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config_file(), config).unwrap();
        let mut opts = DaemonOptions::new(paths.clone());
        opts.store_key = Some(StoreKey::from_bytes([7; 32]));
        opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
        opts.runtime = runtime;
        let daemon = match Daemon::start(opts).await.unwrap() {
            Startup::Ready(d) => d,
            Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
        };
        let runtime = daemon.runtime();
        let (tx, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(daemon.run_until(async {
            let _ = rx.await;
        }));
        let client = Client::connect(&paths).await.unwrap();
        Self {
            tmp,
            client,
            runtime,
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

    async fn search(&self, q: &str) -> Vec<SearchHit> {
        self.call(method::SEARCH, json!({"query": q, "limit": 10}))
            .await
    }

    /// Index two boiler notes; full-text ranks `a` (most term hits) first.
    async fn add_notes(&self) {
        let docs = self.tmp.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("a.md"),
            "Boiler boiler boiler: the boiler manual.",
        )
        .unwrap();
        std::fs::write(
            docs.join("b.md"),
            "Boiler repair: fault F28, ignition electrode replaced.",
        )
        .unwrap();
        let _: SourceInfo = self
            .call(
                method::SOURCES_ADD,
                json!({"kind": "files", "name": "Docs", "settings": {"roots": [docs]}}),
            )
            .await;
        eventually("both notes searchable", || async {
            self.search("boiler").await.len() == 2
        })
        .await;
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn first_is_repair(hits: &[SearchHit]) -> bool {
    hits.first()
        .is_some_and(|h| h.snippet.contains("electrode"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_external_reranker_reorders_search_results() {
    let reranker = MockServer::start().await;
    let config = format!(
        "[runtime]\nenabled = false\n[retrieve]\nmmr = false\n\
         [retrieve.reranker]\nbase_url = \"{}\"\n",
        reranker.uri()
    );
    let mut h = Harness::start(&config, RuntimeOverrides::default()).await;
    h.add_notes().await;

    // The reranker answers 404 (nothing mounted): first-stage order.
    let hits = h.search("boiler").await;
    assert_eq!(hits.len(), 2);
    assert!(!first_is_repair(&hits), "{hits:?}");

    // Now it prefers the passage about the electrode.
    Mock::given(http_method("POST"))
        .and(path("/v1/rerank"))
        .respond_with(|req: &Request| {
            let v: Value = serde_json::from_slice(&req.body).unwrap();
            let results: Vec<Value> = v["documents"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let s = if d.as_str().unwrap().contains("electrode") {
                        0.95
                    } else {
                        0.05
                    };
                    json!({"index": i, "relevance_score": s})
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"results": results}))
        })
        .mount(&reranker)
        .await;
    let hits = h.search("boiler").await;
    assert_eq!(hits.len(), 2);
    assert!(first_is_repair(&hits), "{hits:?}");
    h.stop().await;
}

fn fake_server() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nuggetd-fake-llama-server"))
}

const REV: &str = "0123456789abcdef0123456789abcdef01234567";

/// A manifest with one tiny "reranker" served by `server`.
async fn rerank_manifest(server: &MockServer) -> Manifest {
    let id = "rerank-model";
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
            // The manifest has no rerank tier; the entry is only used because it is configured.
            tiers: vec![Tier::Embedding],
            license: "Apache-2.0".into(),
            min_app_version: "0.0.1".into(),
        }],
        runtimes: vec![],
    }
}

/// Before its model is installed the local reranker is skipped at once (no wait for the
/// rerank timeout); once installed its server starts, never joins the router, and search
/// sends it the candidates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_local_rerank_runtime_is_used_once_installed() {
    let models_server = MockServer::start().await;
    let overrides = RuntimeOverrides {
        server_binary: Some((fake_server(), RuntimeAccel::Cpu)),
        options: Some(RuntimeOptions {
            health_timeout: Duration::from_secs(15),
            health_poll: Duration::from_millis(20),
            watch_interval: Duration::from_millis(50),
            shutdown_grace: Duration::from_secs(2),
            ..RuntimeOptions::default()
        }),
        manifest: Some(rerank_manifest(&models_server).await),
        hardware: Some(Hardware {
            total_ram: 16 << 30,
            available_ram: 8 << 30,
            cpu_cores: 4,
            gpu: GpuKind::Unknown,
        }),
    };
    // Chat and embedding models that are not in the manifest: only the reranker is selected.
    let config = "[runtime]\nchat_model = \"absent-chat\"\nembedding_model = \"absent-embed\"\n\
                  rerank_model = \"rerank-model\"\nidle_stop_minutes = 0\n\
                  [retrieve]\nmmr = false\nrerank_timeout_ms = 20000\n";
    let mut h = Harness::start(config, overrides).await;
    let rt: Vec<RuntimeInfo> = h.call(method::RUNTIME_STATUS, json!({})).await;
    let rr = rt.iter().find(|r| r.role == RuntimeRole::Rerank).unwrap();
    assert_eq!(rr.state, RuntimeState::NotInstalled);
    assert_eq!(rr.provider.as_str(), "local-rerank");
    h.add_notes().await;

    let t = Instant::now();
    assert_eq!(h.search("boiler").await.len(), 2);
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "search waited for a reranker that is not there: {:?}",
        t.elapsed()
    );

    let _: Value = h
        .call(method::MODELS_INSTALL, json!({"id": "rerank-model"}))
        .await;
    eventually("rerank runtime ready", || async {
        let rt: Vec<RuntimeInfo> = h.call(method::RUNTIME_STATUS, json!({})).await;
        rt.iter()
            .any(|r| r.role == RuntimeRole::Rerank && r.state == RuntimeState::Ready)
    })
    .await;
    // Not a router provider.
    let providers: Vec<nugget_core::ipc::ProviderInfo> =
        h.call(method::PROVIDERS_LIST, json!({})).await;
    assert!(
        providers.iter().all(|p| p.tiers.is_empty()),
        "{providers:?}"
    );

    assert_eq!(h.search("boiler electrode").await.len(), 2);
    // The server scored the candidates (its prompt-token counter moved).
    let runtime = h.runtime.clone().expect("runtime enabled");
    let d = runtime.details(RuntimeRole::Rerank).unwrap();
    let cfg = d.config.expect("ready config");
    let key = runtime
        .secrets()
        .resolve(&nugget_runtime::api_key_ref(RuntimeRole::Rerank))
        .unwrap();
    let mut b = reqwest::Client::builder().no_proxy();
    #[cfg(unix)]
    if let Some(sock) = &cfg.socket_path {
        b = b.unix_socket(sock.clone());
    }
    let base = cfg.base_url.clone().unwrap();
    let metrics = b
        .build()
        .unwrap()
        .get(format!("{}/metrics", base.trim_end_matches("/v1")))
        .bearer_auth(key.expose())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let tokens: f64 = metrics
        .lines()
        .find_map(|l| l.strip_prefix("llamacpp:prompt_tokens_total "))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(tokens >= 2.0, "{metrics}");
    h.stop().await;
}
