//! Shared fixtures: a model store with tiny "models" served by wiremock, the fake llama-server,
//! and process helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nugget_core::capability::Tier;
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::paths::Paths;
use nugget_models::{DownloadOptions, GpuKind, Hardware, ModelEntry, ModelStore};
use nugget_runtime::{Runtime, RuntimeOptions};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const REV: &str = "0123456789abcdef0123456789abcdef01234567";

pub fn fake_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-llama-server"))
}

#[derive(Default)]
pub struct VecSink(pub Mutex<Vec<EgressEvent>>);

impl EgressSink for VecSink {
    fn record(&self, event: EgressEvent) {
        self.0.lock().unwrap().push(event);
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

pub fn hw() -> Hardware {
    Hardware {
        total_ram: 16 << 30,
        available_ram: 8 << 30,
        cpu_cores: 8,
        gpu: GpuKind::Unknown,
    }
}

/// Fast timings for tests.
pub fn opts() -> RuntimeOptions {
    RuntimeOptions {
        health_timeout: Duration::from_secs(15),
        health_poll: Duration::from_millis(20),
        watch_interval: Duration::from_millis(40),
        restart_backoff_base: Duration::from_millis(50),
        restart_backoff_max: Duration::from_millis(200),
        max_restarts: 3,
        stable_after: Duration::from_secs(60),
        shutdown_grace: Duration::from_secs(2),
        ..RuntimeOptions::default()
    }
}

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub paths: Paths,
    pub store: Arc<ModelStore>,
    pub sink: Arc<VecSink>,
    pub server: MockServer,
    pub chat: ModelEntry,
    pub embed: ModelEntry,
}

pub fn no_proxy_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

pub async fn serve(server: &MockServer, p: &str, bytes: Vec<u8>) -> String {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
        .mount(server)
        .await;
    format!("{}{p}", server.uri())
}

async fn model(server: &MockServer, id: &str, tiers: Vec<Tier>) -> ModelEntry {
    let bytes = format!("GGUF fake weights for {id}").into_bytes();
    let url = serve(
        server,
        &format!("/{id}/resolve/{REV}/{id}.gguf"),
        bytes.clone(),
    )
    .await;
    ModelEntry {
        id: id.into(),
        family: "test".into(),
        file_name: format!("{id}.gguf"),
        url,
        mirrors: vec![],
        revision: REV.into(),
        size: bytes.len() as u64,
        sha256: sha256_hex(&bytes),
        min_ram_gb: 1,
        tiers,
        license: "Apache-2.0".into(),
        min_app_version: "0.0.1".into(),
    }
}

/// A temp `NUGGET_HOME` with a chat and an embedding "model" installed.
pub async fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path().to_path_buf());
    let sink = Arc::new(VecSink::default());
    let store = ModelStore::open(&paths, sink.clone())
        .unwrap()
        .with_client(no_proxy_client())
        .with_download_options(DownloadOptions {
            progress_interval: Duration::ZERO,
            backoff_base: Duration::from_millis(5),
            ..DownloadOptions::default()
        });
    let server = MockServer::start().await;
    let chat = model(
        &server,
        "chat-model",
        vec![Tier::Triage, Tier::Light, Tier::Embedding],
    )
    .await;
    let embed = model(&server, "embed-model", vec![Tier::Embedding]).await;
    store.install(&chat, |_| {}).await.unwrap();
    store.install(&embed, |_| {}).await.unwrap();
    Env {
        tmp,
        paths,
        store: Arc::new(store),
        sink,
        server,
        chat,
        embed,
    }
}

impl Env {
    /// A runtime that runs the fake server.
    pub fn runtime(&self, opts: RuntimeOptions) -> Runtime {
        Runtime::new(&self.paths, self.store.clone(), self.sink.clone())
            .unwrap()
            .with_hardware(hw())
            .with_options(opts)
            .with_server_binary(fake_bin(), nugget_models::RuntimeAccel::Cpu)
    }
}

/// Whether `pid` is a live (non-zombie) process.
pub fn alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid)
        .is_some_and(|p| !matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead))
}

pub async fn wait_until(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < timeout, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub async fn get(port: u16, p: &str, key: Option<&str>) -> (u16, String) {
    let mut req = no_proxy_client().get(format!("http://127.0.0.1:{port}{p}"));
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    match req.send().await {
        Ok(r) => (r.status().as_u16(), r.text().await.unwrap_or_default()),
        Err(_) => (0, String::new()),
    }
}

pub fn port_of(base_url: &str) -> u16 {
    base_url
        .trim_start_matches("http://127.0.0.1:")
        .trim_end_matches("/v1")
        .parse()
        .unwrap()
}

pub fn read_all_logs(dir: &Path) -> String {
    let mut out = String::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            out.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
        }
    }
    out
}
