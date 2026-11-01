//! `models.cancel`: a running download stops, reports `failed` / `cancelled` with the staged
//! byte count, and the next `models.install` resumes from those bytes (HTTP Range).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use nugget_core::capability::Tier;
use nugget_core::ipc::{
    ModelCancelResponse, ModelFailReason, ModelInfo, ModelPhase, ModelProgress, method,
    notification,
};
use nugget_core::paths::Paths;
use nugget_ingest::MemorySecrets;
use nugget_ipc::{Client, Notification};
use nugget_models::{Manifest, ModelEntry};
use nugget_store::StoreKey;
use nuggetd::{Daemon, DaemonOptions, RuntimeOverrides, Startup};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const CALL: Duration = Duration::from_secs(30);
const EVENTUALLY: Duration = Duration::from_secs(30);
const REV: &str = "0123456789abcdef0123456789abcdef01234567";
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

fn manifest(url: &str, bytes: &[u8]) -> Manifest {
    Manifest {
        version: 1,
        models: vec![ModelEntry {
            id: "m".into(),
            family: "test".into(),
            file_name: "m.gguf".into(),
            url: url.into(),
            mirrors: vec![],
            revision: REV.into(),
            size: bytes.len() as u64,
            sha256: nugget_store::content_hash(bytes),
            min_ram_gb: 1,
            tiers: vec![Tier::Triage],
            license: "Apache-2.0".into(),
            min_app_version: "0.0.1".into(),
        }],
        runtimes: vec![],
    }
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + EVENTUALLY;
    while !f() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn progress(notes: &Arc<Mutex<Vec<Notification>>>) -> Vec<ModelProgress> {
    notes
        .lock()
        .unwrap()
        .iter()
        .filter(|n| n.method == notification::MODELS_PROGRESS)
        .filter_map(|n| n.params_as::<ModelProgress>().ok())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_keeps_staged_bytes_and_the_next_install_resumes() {
    let bytes: Vec<u8> = (0..SIZE).map(|i| (i * 31 % 251) as u8).collect();
    let srv = range_server(Arc::new(bytes.clone())).await;

    let tmp = tempfile::Builder::new()
        .prefix("nugget-mc-")
        .tempdir()
        .unwrap();
    let paths = Paths::rooted(tmp.path().join("home"));
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.config_file(), "[runtime]\nenabled = false\n").unwrap();
    let mut opts = DaemonOptions::new(paths.clone());
    opts.store_key = Some(StoreKey::from_bytes([5; 32]));
    opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
    opts.runtime = RuntimeOverrides {
        manifest: Some(manifest(&srv.url, &bytes)),
        ..RuntimeOverrides::default()
    };
    let daemon = match Daemon::start(opts).await.unwrap() {
        Startup::Ready(d) => d,
        Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
    };
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(daemon.run_until(async {
        let _ = stopped.await;
    }));
    let client = Client::connect(&paths).await.unwrap();
    let call = async |m: &str, params: Value| -> Value {
        tokio::time::timeout(CALL, client.call(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} failed: {e}"))
    };
    let notes = Arc::new(Mutex::new(Vec::new()));
    {
        let mut stream = client.notifications();
        let n = notes.clone();
        tokio::spawn(async move {
            while let Some(x) = stream.next().await {
                n.lock().unwrap().push(x);
            }
        });
    }

    // Nothing running yet.
    let r: ModelCancelResponse =
        serde_json::from_value(call(method::MODELS_CANCEL, json!({"id": "m"})).await).unwrap();
    assert!(!r.cancelled);

    call(method::MODELS_INSTALL, json!({"id": "m"})).await;
    eventually("bytes staged", || {
        progress(&notes)
            .iter()
            .any(|p| p.phase == ModelPhase::Downloading && p.downloaded >= 1024 * 1024)
    })
    .await;
    let r: ModelCancelResponse =
        serde_json::from_value(call(method::MODELS_CANCEL, json!({"id": "m"})).await).unwrap();
    assert!(r.cancelled, "progress so far: {:?}", progress(&notes));
    eventually("cancelled progress", || {
        progress(&notes)
            .iter()
            .any(|p| p.phase == ModelPhase::Failed)
    })
    .await;
    let failed = progress(&notes)
        .into_iter()
        .find(|p| p.phase == ModelPhase::Failed)
        .unwrap();
    assert_eq!(failed.reason, Some(ModelFailReason::Cancelled));
    // Committed in 1 MiB steps; whatever was committed is reported and kept.
    let staged = failed.downloaded;
    assert!(
        (1024 * 1024..=STALL_AT as u64).contains(&staged),
        "staged {staged}"
    );
    assert!(!failed.error.unwrap_or_default().contains("http"));

    // Resume: one ranged request from the staged offset, and the install verifies.
    srv.stall.store(false, Ordering::SeqCst);
    let before = srv.ranges.lock().unwrap().len();
    notes.lock().unwrap().clear();
    // The cancelled install releases its slot right after its final progress.
    let mut started = false;
    for _ in 0..50 {
        match tokio::time::timeout(
            CALL,
            client.call::<Value, Value>(method::MODELS_INSTALL, &json!({"id": "m"})),
        )
        .await
        {
            Ok(Ok(_)) => {
                started = true;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    assert!(started, "second install could not start");
    eventually("installed", || {
        progress(&notes)
            .iter()
            .any(|p| p.phase == ModelPhase::Installed)
    })
    .await;
    let models: Vec<ModelInfo> =
        serde_json::from_value(call(method::MODELS_LIST, json!({})).await).unwrap();
    assert!(models.iter().any(|m| m.id == "m" && m.installed));
    let after: Vec<String> = srv.ranges.lock().unwrap()[before..].to_vec();
    assert_eq!(
        after,
        vec![
            "bytes=0-0".to_string(),
            format!("bytes={staged}-{}", SIZE - 1)
        ]
    );

    let _ = stop.send(());
    tokio::time::timeout(CALL, server)
        .await
        .expect("daemon stops promptly")
        .unwrap()
        .unwrap();
}
