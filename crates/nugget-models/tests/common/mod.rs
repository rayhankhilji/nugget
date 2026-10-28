//! Test fixtures: a scriptable HTTP/1.1 range server on 127.0.0.1, signing helpers, fakes.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_models::{
    DiskSpace, DownloadOptions, Manifest, ModelEntry, ModelStore, SigningKey, sign_manifest,
    verify_manifest,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const REV: &str = "0123456789abcdef0123456789abcdef01234567";

/// A request as the server saw it.
#[derive(Debug, Clone)]
pub struct Req {
    pub path: String,
    /// `(start, end_inclusive)`; `None` end means open-ended.
    pub range: Option<(u64, Option<u64>)>,
}

/// What to do with a request.
#[derive(Debug, Clone)]
pub enum Action {
    Serve,
    Status(u16),
    /// Promise the full range, send only `n` body bytes, then close the connection.
    DropAfter(usize),
    /// A valid 206 covering only the first `n` bytes of the requested range.
    Short(usize),
    /// Answer 200 with the whole file regardless of `Range`.
    IgnoreRange,
    Redirect(String),
}

pub type Behavior = Arc<dyn Fn(&Req) -> Action + Send + Sync>;

pub struct TestServer {
    pub base: String,
    files: Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>,
    behavior: Arc<Mutex<Behavior>>,
    requests: Arc<Mutex<Vec<Req>>>,
    delay: Arc<Mutex<Duration>>,
    inflight: Arc<AtomicUsize>,
    max_inflight: Arc<AtomicUsize>,
}

impl TestServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let s = Self {
            base,
            files: Arc::default(),
            behavior: Arc::new(Mutex::new(Arc::new(|_: &Req| Action::Serve))),
            requests: Arc::default(),
            delay: Arc::default(),
            inflight: Arc::default(),
            max_inflight: Arc::default(),
        };
        let (files, behavior, requests, delay, inflight, max_inflight) = (
            s.files.clone(),
            s.behavior.clone(),
            s.requests.clone(),
            s.delay.clone(),
            s.inflight.clone(),
            s.max_inflight.clone(),
        );
        tokio::spawn(async move {
            loop {
                let Ok((conn, _)) = listener.accept().await else {
                    return;
                };
                let st = Conn {
                    files: files.clone(),
                    behavior: behavior.clone(),
                    requests: requests.clone(),
                    delay: delay.clone(),
                    inflight: inflight.clone(),
                    max_inflight: max_inflight.clone(),
                };
                tokio::spawn(async move { st.handle(conn).await });
            }
        });
        s
    }

    /// Serve `data` at `/<name>/resolve/<REV>/<name>.gguf`; returns the URL.
    pub fn add(&self, name: &str, data: Vec<u8>) -> String {
        let path = format!("/{name}/resolve/{REV}/{name}.gguf");
        self.files
            .lock()
            .unwrap()
            .insert(path.clone(), Arc::new(data));
        format!("{}{path}", self.base)
    }

    pub fn set_behavior(&self, f: impl Fn(&Req) -> Action + Send + Sync + 'static) {
        *self.behavior.lock().unwrap() = Arc::new(f);
    }

    pub fn set_delay(&self, d: Duration) {
        *self.delay.lock().unwrap() = d;
    }

    pub fn requests(&self) -> Vec<Req> {
        self.requests.lock().unwrap().clone()
    }

    pub fn clear_requests(&self) {
        self.requests.lock().unwrap().clear();
    }

    pub fn max_inflight(&self) -> usize {
        self.max_inflight.load(Ordering::SeqCst)
    }
}

struct Conn {
    files: Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>,
    behavior: Arc<Mutex<Behavior>>,
    requests: Arc<Mutex<Vec<Req>>>,
    delay: Arc<Mutex<Duration>>,
    inflight: Arc<AtomicUsize>,
    max_inflight: Arc<AtomicUsize>,
}

impl Conn {
    async fn handle(self, mut conn: TcpStream) {
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match conn.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        let text = String::from_utf8_lossy(&head).to_string();
        let mut lines = text.split("\r\n");
        let path = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        let range = lines
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("range"))
            .and_then(|(_, v)| parse_range(v.trim()));
        let req = Req { path, range };
        self.requests.lock().unwrap().push(req.clone());

        let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_inflight.fetch_max(now, Ordering::SeqCst);
        let delay = *self.delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let action = (self.behavior.lock().unwrap().clone())(&req);
        let file = self.files.lock().unwrap().get(&req.path).cloned();
        let _ = respond(&mut conn, &req, action, file).await;
        let _ = conn.shutdown().await;
        self.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

fn parse_range(v: &str) -> Option<(u64, Option<u64>)> {
    let (s, e) = v.strip_prefix("bytes=")?.split_once('-')?;
    Some((s.parse().ok()?, e.parse().ok()))
}

async fn respond(
    conn: &mut TcpStream,
    req: &Req,
    action: Action,
    file: Option<Arc<Vec<u8>>>,
) -> std::io::Result<()> {
    let head = |status: &str, extra: &str, len: usize| {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n{extra}\r\n"
        )
    };
    if let Action::Status(code) = action {
        conn.write_all(head(&format!("{code} X"), "", 0).as_bytes())
            .await?;
        return Ok(());
    }
    if let Action::Redirect(loc) = &action {
        conn.write_all(head("302 Found", &format!("Location: {loc}\r\n"), 0).as_bytes())
            .await?;
        return Ok(());
    }
    let Some(data) = file else {
        conn.write_all(head("404 Not Found", "", 0).as_bytes())
            .await?;
        return Ok(());
    };
    let total = data.len() as u64;
    let ranged = match (&action, req.range) {
        (Action::IgnoreRange, _) | (_, None) => None,
        (_, Some((s, e))) => Some((s, e.unwrap_or(total - 1).min(total - 1))),
    };
    let Some((start, mut end)) = ranged else {
        conn.write_all(head("200 OK", "", data.len()).as_bytes())
            .await?;
        return conn.write_all(&data).await;
    };
    if start >= total {
        let h = head(
            "416 Range Not Satisfiable",
            &format!("Content-Range: bytes */{total}\r\n"),
            0,
        );
        return conn.write_all(h.as_bytes()).await;
    }
    if let Action::Short(n) = action {
        end = end.min(start + n as u64 - 1);
    }
    let body = &data[start as usize..=end as usize];
    let h = head(
        "206 Partial Content",
        &format!("Content-Range: bytes {start}-{end}/{total}\r\n"),
        body.len(),
    );
    conn.write_all(h.as_bytes()).await?;
    match action {
        Action::DropAfter(n) => {
            conn.write_all(&body[..n.min(body.len())]).await?;
            conn.flush().await
        }
        _ => conn.write_all(body).await,
    }
}

// ---- data, signing, entries ----

/// Deterministic pseudo-random bytes.
pub fn data(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

pub fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

pub fn entry(id: &str, url: &str, bytes: &[u8]) -> ModelEntry {
    ModelEntry {
        id: id.into(),
        family: "test".into(),
        file_name: format!("{id}.gguf"),
        url: url.into(),
        mirrors: vec![],
        revision: REV.into(),
        size: bytes.len() as u64,
        sha256: sha256_hex(bytes),
        min_ram_gb: 1,
        tiers: vec![nugget_core::capability::Tier::Triage],
        license: "Apache-2.0".into(),
        min_app_version: "0.0.1".into(),
    }
}

/// Round-trip entries through a signed manifest, as production code must.
pub fn signed(models: Vec<ModelEntry>) -> Manifest {
    let bytes = serde_json::to_vec_pretty(&Manifest { version: 1, models }).unwrap();
    let key = test_key();
    let sig = sign_manifest(&bytes, &key);
    verify_manifest(&bytes, sig.as_bytes(), &[key.verifying_key()]).unwrap()
}

// ---- fakes ----

#[derive(Default)]
pub struct VecSink(pub Mutex<Vec<EgressEvent>>);

impl EgressSink for VecSink {
    fn record(&self, event: EgressEvent) {
        self.0.lock().unwrap().push(event);
    }
}

impl VecSink {
    pub fn received(&self) -> u64 {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.bytes_received)
            .sum()
    }
}

pub struct FakeDisk(pub Option<u64>);

impl DiskSpace for FakeDisk {
    fn available_bytes(&self, _: &Path) -> std::io::Result<u64> {
        self.0
            .ok_or_else(|| std::io::Error::other("statvfs: permission denied"))
    }
}

pub fn fast_opts() -> DownloadOptions {
    DownloadOptions {
        parts: 4,
        min_part_size: 1024,
        max_retries: 3,
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(50),
        commit_every: 512,
        persist_every: 1024,
        progress_interval: Duration::ZERO,
    }
}

pub fn store(root: &Path, sink: Arc<VecSink>) -> ModelStore {
    ModelStore::new(root.join("models"), sink)
        .unwrap()
        .with_client(reqwest::Client::builder().no_proxy().build().unwrap())
        .with_disk_space(Arc::new(FakeDisk(Some(u64::MAX / 2))))
        .with_download_options(fast_opts())
}
