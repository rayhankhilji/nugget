//! The on-device runtime end to end, against the fake `llama-server`: install, routing, restarts,
//! the chat slot while its model loads, fault handling, the local reranker, and cancelling and
//! resuming a model download.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use nugget_core::capability::{Locality, Tier};
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ipc::{
    IngestStatus, ModelCancelResponse, ModelFailReason, ModelInfo, ModelPhase, ModelProgress,
    ProviderInfo, RuntimeRole, RuntimeState, method, notification,
};
use nugget_core::paths::Paths;
use nugget_infer::SecretResolver;
use nugget_ipc::Notification;
use nugget_models::{Manifest, ModelStore};
use nugget_runtime::RuntimeOptions;
use nuggetd::RuntimeOverrides;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::MockServer;
use wiremock::matchers::method as http_method;

const CHAT: &[Tier] = &[Tier::Triage, Tier::Light, Tier::Synthesis];
const BOTH: &str = "[runtime]\nchat_model = \"chat-model\"\nembedding_model = \"embed-model\"\n\
                    idle_stop_minutes = 0\n";
const CHAT_ONLY: &str = "[runtime]\nchat_model = \"chat-model\"\n\
                         embedding_model = \"absent-embed\"\nidle_stop_minutes = 0\n";

struct NoEgress;
impl EgressSink for NoEgress {
    fn record(&self, _event: EgressEvent) {}
}

async fn chat_and_embed(server: &MockServer) -> Manifest {
    manifest(
        server,
        &[("chat-model", CHAT), ("embed-model", &[Tier::Embedding])],
    )
    .await
}

/// Installs `entry` into the home under `tmp` before the daemon starts.
async fn preinstall(tmp: &tempfile::TempDir, entry: &nugget_models::ModelEntry) {
    let store = ModelStore::open(&Paths::rooted(tmp.path().join("home")), Arc::new(NoEgress));
    store.unwrap().install(entry, |_| {}).await.unwrap();
}

async fn start(config: &str, o: nuggetd::RuntimeOverrides) -> Harness {
    Harness::start_with(config, |opts| opts.runtime = o).await
}

async fn ready(h: &Harness, role: RuntimeRole) -> bool {
    h.runtime_state(role)
        .await
        .is_some_and(|r| r.state == RuntimeState::Ready)
}

fn pid_files_gone(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |d| {
        d.filter_map(|e| e.ok())
            .all(|e| e.path().extension().is_none_or(|x| x != "pid"))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_runtime_serves_embeddings_and_chat_and_starts_after_install() {
    let models = MockServer::start().await;
    let manifest = chat_and_embed(&models).await;
    let tmp = tempfile::tempdir().unwrap();
    preinstall(&tmp, manifest.get("embed-model").unwrap()).await;
    let o = overrides(manifest, fast_runtime(&[]));
    let mut h = Harness::start_in(tmp, BOTH, |opts| opts.runtime = o).await;

    eventually("embedding runtime ready", || {
        ready(&h, RuntimeRole::Embedding)
    })
    .await;
    let chat = h.runtime_state(RuntimeRole::Chat).await.unwrap();
    assert_eq!(chat.state, RuntimeState::NotInstalled);
    assert_eq!(chat.provider.as_str(), "local-chat");
    let providers: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    assert_eq!(providers.len(), 1, "only installed runtimes: {providers:?}");
    assert_eq!(providers[0].id.as_str(), "local-embedding");
    assert_eq!(providers[0].locality, Locality::OnDevice);
    assert_eq!(providers[0].tiers, vec![Tier::Embedding]);

    // Indexed text is embedded by the local server.
    h.add_files("docs", &[("n.md", "Saffron risotto needs patience.")])
        .await;
    eventually("embedded file searchable", || async {
        !h.search("saffron").await.is_empty()
    })
    .await;
    assert_eq!(h.store.embedding_dim().unwrap(), Some(8));

    // Installing the chat model starts its server and routes chat tiers to it.
    let watcher = collect(&h.client);
    h.install("chat-model").await;
    eventually("chat runtime ready", || ready(&h, RuntimeRole::Chat)).await;
    eventually("triage routed locally", || async {
        let st: IngestStatus = h.call(method::INGEST_STATUS, json!({})).await;
        st.triage_provider.as_ref().map(|p| p.as_str()) == Some("local-chat")
    })
    .await;
    let (started, text) = h.chat(&watcher, chat_params("light", "hi")).await;
    assert_eq!(started.unwrap().provider.as_str(), "local-chat");
    assert!(!text.unwrap().is_empty());

    h.stop().await;
    assert!(pid_files_gone(&h.paths.data.join("runtime")));
}

/// An update of a model whose server runs restarts the server on the new weights; items
/// indexed before the embedder was installed get vectors once it is ready.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_updates_restart_the_server_and_late_embedders_reembed() {
    let models = MockServer::start().await;
    let manifest = chat_and_embed(&models).await;
    let old = model_entry(
        &models,
        "embed-model",
        b"GGUF older",
        "old-",
        &[Tier::Embedding],
    );
    let tmp = tempfile::tempdir().unwrap();
    preinstall(&tmp, &old.await).await;
    let o = overrides(manifest.clone(), fast_runtime(&[]));
    let mut h = Harness::start_in(tmp, BOTH, |opts| opts.runtime = o).await;
    let runtime = h.runtime.clone().expect("runtime enabled");
    eventually("embedding runtime ready", || {
        ready(&h, RuntimeRole::Embedding)
    })
    .await;
    let before = runtime.details(RuntimeRole::Embedding).unwrap().pid;
    let list: Vec<ModelInfo> = h.call(method::MODELS_LIST, json!({})).await;
    assert!(
        list.iter()
            .any(|m| m.id == "embed-model" && m.update_available)
    );
    let _: Value = h
        .call(method::MODELS_INSTALL, json!({"id": "embed-model"}))
        .await;
    eventually("server restarted on the new weights", || async {
        runtime
            .details(RuntimeRole::Embedding)
            .is_some_and(|d| d.info.state == RuntimeState::Ready && d.pid != before)
    })
    .await;
    h.stop().await;

    // A fresh home with nothing installed: files are indexed text-only first.
    let o = overrides(manifest, fast_runtime(&[]));
    let mut h = start(BOTH, o).await;
    h.add_files("docs", &[("n.md", "Saffron risotto needs patience.")])
        .await;
    eventually("text-only file searchable", || async {
        !h.search("saffron").await.is_empty()
    })
    .await;
    assert_eq!(h.store.embedding_dim().unwrap(), None);
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
    assert!(!h.store.search_vector(&[0.5; 8], 5).unwrap().is_empty());
    h.stop().await;
}

/// QA: the first chat right after onboarding installed the local models failed with "no
/// provider for tier Synthesis has the required capabilities" because the chat slot was
/// routable for nothing until its server first became ready. And when that start fails, chat
/// still falls back to the next provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chat_sent_while_the_chat_model_loads_waits_or_falls_back() {
    let models = MockServer::start().await;
    let slow = |health_timeout| RuntimeOptions {
        health_timeout,
        max_restarts: 0,
        ..fast_runtime(&["--fake-start-delay-ms", "5000"])
    };
    let chat_model = manifest(&models, &[("chat-model", CHAT)]).await;
    let mut h = start(
        CHAT_ONLY,
        overrides(chat_model.clone(), slow(Duration::from_secs(30))),
    )
    .await;
    let watcher = collect(&h.client);
    h.install("chat-model").await;
    eventually("chat server starting", || async {
        h.runtime_state(RuntimeRole::Chat).await.unwrap().state == RuntimeState::Starting
    })
    .await;
    let (started, text) = h.chat(&watcher, chat_params("synthesis", "hi")).await;
    assert_eq!(started.unwrap().provider.as_str(), "local-chat");
    assert_eq!(text.unwrap(), "Hello from fake");
    h.stop().await;

    // Loading takes longer than the health timeout: the start fails and the cloud answers.
    let remote = MockServer::start().await;
    wiremock::Mock::given(http_method("POST"))
        .respond_with(sse_text("Hello from remote"))
        .mount(&remote)
        .await;
    let config = format!(
        "{CHAT_ONLY}\n[[providers]]\nid = \"remote\"\nkind = \"openai_compatible\"\n\
         base_url = \"{}/v1\"\nmodel = \"remote-model\"\ntiers = [\"synthesis\"]\n\
         max_retries = 0\nlocality = \"cloud\"\n",
        remote.uri()
    );
    let mut h = start(
        &config,
        overrides(chat_model, slow(Duration::from_millis(1500))),
    )
    .await;
    let watcher = collect(&h.client);
    h.install("chat-model").await;
    let (started, text) = h.chat(&watcher, chat_params("synthesis", "hi")).await;
    assert_eq!(started.unwrap().provider.as_str(), "remote");
    assert_eq!(text.unwrap(), "Hello from remote");
    eventually("the local start gave up", || async {
        h.runtime_state(RuntimeRole::Chat).await.unwrap().state == RuntimeState::Failed
    })
    .await;
    let (started, _) = h.chat(&watcher, chat_params("synthesis", "hi")).await;
    assert_eq!(started.unwrap().provider.as_str(), "remote");
    h.stop().await;
}

/// The fake server with `args` and the runtime's default watchdog and backoff timings (the
/// 15 s measure is about the shipped defaults).
async fn faulty(args: &[&str]) -> Harness {
    let models = MockServer::start().await;
    let o = RuntimeOptions {
        health_timeout: Duration::from_secs(20),
        health_poll: Duration::from_millis(20),
        chat: fast_runtime(args).chat,
        ..RuntimeOptions::default()
    };
    let h = start(
        CHAT_ONLY,
        overrides(manifest(&models, &[("chat-model", CHAT)]).await, o),
    )
    .await;
    h.install("chat-model").await;
    h
}

fn launches(h: &Harness) -> usize {
    let log = h.paths.data.join("logs").join("llama-server-chat.log");
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .matches("] starting chat-model")
        .count()
}

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

/// KK1 (GG6-3, GG6-17, GG6-5): the local runtime never leaves a chat stuck.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_model_faults_never_leave_a_chat_stuck() {
    // GG6-3: a server that never answers fails the chat within 15 s and is restarted.
    let mut h = faulty(&["--fake-hang-on-chat"]).await;
    let watcher = collect(&h.client);
    eventually("chat model ready", || ready(&h, RuntimeRole::Chat)).await;
    let t = Instant::now();
    let err = h
        .chat(&watcher, chat_params("synthesis", "hi"))
        .await
        .1
        .unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(15), "{:?}", t.elapsed());
    assert!(err.contains("stopped responding"), "{err}");
    assert_plain(&err);
    eventually("restarted", || ready(&h, RuntimeRole::Chat)).await;
    assert_eq!(launches(&h), 2);
    h.stop().await;

    // GG6-17: a crash before the first token is retried once on the restarted server.
    let marker = tempfile::tempdir().unwrap();
    let marker = marker.path().join("crashed");
    let mut h = faulty(&["--fake-crash-on-chat-once", marker.to_str().unwrap()]).await;
    let watcher = collect(&h.client);
    eventually("chat model ready", || ready(&h, RuntimeRole::Chat)).await;
    let (started, res) = h.chat(&watcher, chat_params("synthesis", "hi")).await;
    assert_eq!(res.as_deref(), Ok("Hello from fake"));
    assert_eq!(started.unwrap().provider.as_str(), "local-chat");
    assert!(marker.exists(), "the server did crash once");
    assert_eq!(launches(&h), 2);
    h.stop().await;

    // GG6-5: a server that cannot start fails fast, without a relaunch per request.
    let mut h = faulty(&["--fake-exit-immediately", "1"]).await;
    let watcher = collect(&h.client);
    eventually("start failed", || async {
        h.runtime_state(RuntimeRole::Chat).await.unwrap().state == RuntimeState::Failed
    })
    .await;
    let info = h.runtime_state(RuntimeRole::Chat).await.unwrap();
    let error = info.error.unwrap();
    assert!(error.contains("couldn't start"), "{error}");
    assert_plain(&error);
    assert_eq!(
        info.detail.as_deref(),
        Some("fake: exiting immediately with 1")
    );
    let launched = launches(&h);
    for _ in 0..3 {
        let t = Instant::now();
        let err = h
            .chat(&watcher, chat_params("synthesis", "hi"))
            .await
            .1
            .unwrap_err();
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert_plain(&err);
        assert_eq!(err.matches("couldn't start").count(), 1, "{err}");
    }
    assert!(launches(&h) <= launched + 1, "no relaunch per request");
    let before = launches(&h);
    let r: Value = h.call("runtime.retry", json!({"role": "chat"})).await;
    assert_eq!(r["started"], true);
    eventually("retried", || async { launches(&h) > before }).await;
    h.stop().await;
}

/// Before its model is installed the local reranker is skipped at once; once installed its
/// server starts, never joins the router, and search sends it the candidates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_local_rerank_runtime_is_used_once_installed() {
    let models = MockServer::start().await;
    let o = overrides(
        manifest(&models, &[("rerank-model", &[Tier::Rerank])]).await,
        fast_runtime(&[]),
    );
    let config = "[runtime]\nchat_model = \"absent-chat\"\nembedding_model = \"absent-embed\"\n\
                  rerank_model = \"rerank-model\"\nidle_stop_minutes = 0\n\
                  [retrieve]\nmmr = false\nrerank_timeout_ms = 20000\n";
    let mut h = start(config, o).await;
    let rr = h.runtime_state(RuntimeRole::Rerank).await.unwrap();
    assert_eq!(rr.state, RuntimeState::NotInstalled);
    assert_eq!(rr.provider.as_str(), "local-rerank");
    h.add_files(
        "docs",
        &[("a.md", "Boiler manual."), ("b.md", "Boiler electrode.")],
    )
    .await;
    eventually("both notes searchable", || async {
        h.search("boiler").await.len() == 2
    })
    .await;
    let t = Instant::now();
    assert_eq!(h.search("boiler").await.len(), 2);
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());

    h.install("rerank-model").await;
    eventually("rerank runtime ready", || ready(&h, RuntimeRole::Rerank)).await;
    let providers: Vec<ProviderInfo> = h.call(method::PROVIDERS_LIST, json!({})).await;
    for p in &providers {
        let want = if p.id.as_str() == "local-rerank" {
            vec![Tier::Rerank]
        } else {
            vec![]
        };
        assert_eq!(p.tiers, want, "{providers:?}");
    }
    assert_eq!(h.search("boiler electrode").await.len(), 2);

    // The server scored the candidates (its prompt-token counter moved).
    let runtime = h.runtime.clone().unwrap();
    let cfg = runtime
        .details(RuntimeRole::Rerank)
        .unwrap()
        .config
        .unwrap();
    let key = runtime
        .secrets()
        .resolve(&nugget_runtime::api_key_ref(RuntimeRole::Rerank))
        .unwrap();
    let b = reqwest::Client::builder().no_proxy();
    #[cfg(unix)]
    let b = match &cfg.socket_path {
        Some(sock) => b.unix_socket(sock.clone()),
        None => b,
    };
    let base = cfg.base_url.unwrap();
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

const SIZE: usize = 3 * 1024 * 1024;
/// The first download stalls after this many bytes (whole 1 MiB commits).
const STALL_AT: usize = 2 * 1024 * 1024;

/// A range server that, while `stall` is set, sends `STALL_AT` bytes of a body and then holds
/// the connection silently. Records every `Range` header.
struct RangeServer {
    url: String,
    stall: Arc<AtomicBool>,
    ranges: Arc<Mutex<Vec<String>>>,
}

async fn range_server(body: Arc<Vec<u8>>) -> RangeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/m/resolve/{REV}/m.gguf",
        listener.local_addr().unwrap()
    );
    let stall = Arc::new(AtomicBool::new(true));
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let (s, r) = (stall.clone(), ranges.clone());
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            let (body, stall, ranges) = (body.clone(), s.clone(), r.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match conn.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let range = text
                    .split("\r\n")
                    .filter_map(|l| l.split_once(':'))
                    .find(|(k, _)| k.trim().eq_ignore_ascii_case("range"))
                    .map(|(_, v)| v.trim().to_string());
                let total = body.len();
                let (start, end) = range
                    .as_deref()
                    .and_then(|v| v.strip_prefix("bytes="))
                    .and_then(|v| v.split_once('-'))
                    .map_or((0, total - 1), |(a, b)| {
                        (a.parse().unwrap(), b.parse().unwrap_or(total - 1))
                    });
                ranges.lock().unwrap().push(range.unwrap_or_default());
                let slice = &body[start..=end];
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\n\
                     Content-Length: {}\r\nContent-Range: bytes {start}-{end}/{total}\r\n\
                     Connection: close\r\n\r\n",
                    slice.len()
                );
                if conn.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                if slice.len() > 1 && stall.load(Ordering::SeqCst) {
                    // Paced, so the (throttled) progress shows the bytes arriving.
                    for chunk in slice[..STALL_AT.min(slice.len())].chunks(64 * 1024) {
                        if conn.write_all(chunk).await.is_err() || conn.flush().await.is_err() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    return;
                }
                let _ = conn.write_all(slice).await;
                let _ = conn.shutdown().await;
            });
        }
    });
    RangeServer { url, stall, ranges }
}

fn progress(notes: &Mutex<Vec<Notification>>) -> Vec<ModelProgress> {
    let notes = notes.lock().unwrap();
    notes
        .iter()
        .filter(|n| n.method == notification::MODELS_PROGRESS)
        .filter_map(|n| n.params_as::<ModelProgress>().ok())
        .collect()
}

/// `models.cancel` stops a running download, reports `failed` / `cancelled` with the staged
/// byte count, and the next `models.install` resumes from those bytes (HTTP Range).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_keeps_staged_bytes_and_the_next_install_resumes() {
    let bytes: Vec<u8> = (0..SIZE).map(|i| (i * 31 % 251) as u8).collect();
    let srv = range_server(Arc::new(bytes.clone())).await;
    let models = MockServer::start().await;
    let mut entry = model_entry(&models, "m", &bytes, "", &[Tier::Triage]).await;
    entry.url = srv.url.clone();
    let manifest = Manifest {
        version: 1,
        models: vec![entry],
        runtimes: vec![],
    };
    let o = RuntimeOverrides {
        manifest: Some(manifest),
        ..RuntimeOverrides::default()
    };
    let mut h = Harness::start_with(NO_RUNTIME, |opts| opts.runtime = o).await;
    let notes = collect(&h.client);
    let cancel =
        async || -> ModelCancelResponse { h.call(method::MODELS_CANCEL, json!({"id": "m"})).await };
    assert!(!cancel().await.cancelled, "nothing running yet");
    let _: Value = h.call(method::MODELS_INSTALL, json!({"id": "m"})).await;
    eventually("bytes staged", || async {
        progress(&notes)
            .iter()
            .any(|p| p.phase == ModelPhase::Downloading && p.downloaded >= 1024 * 1024)
    })
    .await;
    assert!(
        cancel().await.cancelled,
        "progress so far: {:?}",
        progress(&notes)
    );
    let failed = || {
        progress(&notes)
            .into_iter()
            .find(|p| p.phase == ModelPhase::Failed)
    };
    eventually("cancelled progress", || async { failed().is_some() }).await;
    let failed = failed().unwrap();
    assert_eq!(failed.reason, Some(ModelFailReason::Cancelled));
    // Committed in 1 MiB steps; whatever was committed is reported and kept.
    let staged = failed.downloaded;
    assert!(
        (1024 * 1024..=STALL_AT as u64).contains(&staged),
        "staged {staged}"
    );
    assert!(!failed.error.unwrap_or_default().contains("http"));

    // Resume: one ranged request from the staged offset, and the install verifies. The
    // cancelled install releases its slot right after its final progress.
    srv.stall.store(false, Ordering::SeqCst);
    let before = srv.ranges.lock().unwrap().len();
    eventually("second install starts", || async {
        h.try_call(method::MODELS_INSTALL, json!({"id": "m"}))
            .await
            .is_ok()
    })
    .await;
    eventually("installed", || async {
        let list: Vec<ModelInfo> = h.call(method::MODELS_LIST, json!({})).await;
        list.iter().any(|m| m.id == "m" && m.installed)
    })
    .await;
    let after: Vec<String> = srv.ranges.lock().unwrap()[before..].to_vec();
    let want = vec![
        "bytes=0-0".to_string(),
        format!("bytes={staged}-{}", SIZE - 1),
    ];
    assert_eq!(after, want);
    h.stop().await;
}
