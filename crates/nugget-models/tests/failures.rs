//! Download failure classes (fast, plain-language, with staged bytes), cancellation and resume.
mod common;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Action, TestServer, VecSink, data, entry, fast_opts, signed, store};
use nugget_core::Error;
use nugget_core::ipc::{ModelFailReason, ModelProgress};
use nugget_models::{DownloadOptions, FailReason, ModelStore, Phase, Progress};

const SIZE: usize = 64 * 1024;

type Log = Arc<Mutex<Vec<Progress>>>;

fn progress_log() -> (Log, impl Fn(Progress) + Send + Sync + 'static) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    (log, move |p| l.lock().unwrap().push(p))
}

fn last(log: &Log) -> Progress {
    log.lock().unwrap().last().cloned().unwrap()
}

/// The user-facing message names no URL, host or library error text.
fn assert_plain(msg: &str) {
    for bad in [
        "http://",
        "https://",
        "127.0.0.1",
        "reqwest",
        "error sending",
        "bytes",
    ] {
        assert!(!msg.contains(bad), "{bad:?} in user message {msg:?}");
    }
    assert!(msg.len() < 160, "message too long: {msg}");
}

/// Wait (bounded) until `cond` holds.
async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn dead_url(srv: &TestServer, url: &str) -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    url.replace(&srv.base, &format!("http://127.0.0.1:{port}"))
}

#[tokio::test]
async fn offline_fails_fast_with_default_options() {
    // Production retry settings (only the HTTP client is the test's): a refused connection must
    // be reported within seconds, not after the old ~100 s backoff ladder.
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default()).with_download_options(DownloadOptions::default());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 1);
    let url = dead_url(&srv, &srv.add("m", bytes.clone()));
    let m = signed(vec![entry("m", &url, &bytes)]);

    let (log, cb) = progress_log();
    let t0 = Instant::now();
    let err = st.install(&m.models[0], cb).await.unwrap_err();
    let took = t0.elapsed();
    assert!(took < Duration::from_secs(10), "took {took:?}");
    assert!(matches!(err, Error::Network(_)), "{err}");

    let p = last(&log);
    assert_eq!(p.phase, Phase::Failed);
    assert_eq!(p.reason, Some(FailReason::Offline));
    let msg = p.error.clone().unwrap();
    assert!(msg.contains("offline"), "{msg}");
    assert_plain(&msg);
    let ipc = ModelProgress::from(&p);
    assert_eq!(ipc.reason, Some(ModelFailReason::Offline));
    assert_eq!(ipc.http_status, None);
}

#[tokio::test]
async fn http_500_is_retried_briefly_then_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 2);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    srv.set_behavior(|_| Action::Status(500));

    let (log, cb) = progress_log();
    let t0 = Instant::now();
    st.install(&m.models[0], cb).await.unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    // max_retries = 3: at most four attempts.
    assert!(srv.requests().len() <= 4, "{}", srv.requests().len());

    let p = last(&log);
    assert_eq!(p.reason, Some(FailReason::HttpError(500)));
    let msg = p.error.clone().unwrap();
    assert!(msg.contains("HTTP 500"), "{msg}");
    assert_plain(&msg);
    let ipc = ModelProgress::from(&p);
    assert_eq!(
        (ipc.reason, ipc.http_status),
        (Some(ModelFailReason::HttpError), Some(500))
    );
}

#[tokio::test]
async fn http_4xx_is_not_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 3);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    srv.set_behavior(|_| Action::Status(404));

    let (log, cb) = progress_log();
    st.install(&m.models[0], cb).await.unwrap_err();
    assert_eq!(srv.requests().len(), 1);
    assert_eq!(last(&log).reason, Some(FailReason::HttpError(404)));
}

#[tokio::test]
async fn captive_portal_page_is_recognised_and_not_staged() {
    let tmp = tempfile::tempdir().unwrap();
    let sink = Arc::new(VecSink::default());
    let st = store(tmp.path(), sink.clone());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 4);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    srv.set_behavior(|_| Action::Html);

    let (log, cb) = progress_log();
    let t0 = Instant::now();
    st.install(&m.models[0], cb).await.unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert_eq!(srv.requests().len(), 1, "a portal is not retried");

    let p = last(&log);
    assert_eq!(p.reason, Some(FailReason::CaptivePortal));
    assert_eq!(p.done, 0);
    let msg = p.error.clone().unwrap();
    assert!(msg.contains("sign in"), "{msg}");
    assert_plain(&msg);
    assert_eq!(st.staged_bytes(&m.models[0]), 0);
    // The request still went into the egress ledger.
    assert!(!sink.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stall_is_detected_and_keeps_staged_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default()).with_download_options(DownloadOptions {
        stall_timeout: Duration::from_millis(300),
        ..fast_opts()
    });
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 5);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    // Every part sends 2 KiB, then goes silent.
    srv.set_behavior(|r| match r.range {
        Some((s, Some(e))) if e > s => Action::StallAfter(2048),
        _ => Action::Serve,
    });

    let (log, cb) = progress_log();
    let t0 = Instant::now();
    st.install(&m.models[0], cb).await.unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());

    let p = last(&log);
    assert_eq!(p.reason, Some(FailReason::Stalled));
    assert_plain(p.error.as_deref().unwrap());
    assert!(p.done >= 2048, "staged {}", p.done);
    assert_eq!(p.done, st.staged_bytes(&m.models[0]));
}

#[tokio::test]
async fn server_dying_midway_reports_the_staged_bytes_and_retry_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let sink = Arc::new(VecSink::default());
    let st = store(tmp.path(), sink.clone());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 6);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    let e = &m.models[0];

    // Each part's first request delivers half, then the server is gone for good.
    let seen = Mutex::new(HashSet::new());
    srv.set_behavior(move |r| match r.range {
        Some((s, Some(e))) if e > s => {
            // Keyed by the part's end: a resumed request starts elsewhere but ends there too.
            if seen.lock().unwrap().insert(e) {
                Action::DropAfter(((e - s + 1) >> 1) as usize)
            } else {
                Action::Close
            }
        }
        _ => Action::Serve,
    });

    let (log, cb) = progress_log();
    let t0 = Instant::now();
    st.install(e, cb).await.unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    let p = last(&log);
    assert_eq!(p.phase, Phase::Failed);
    assert_eq!(p.reason, Some(FailReason::Interrupted));
    assert_plain(p.error.as_deref().unwrap());
    let staged = st.staged_bytes(e);
    assert_eq!(p.done, staged);
    assert!(staged >= SIZE as u64 / 2 - 4 * 512, "staged {staged}");
    assert_eq!(ModelProgress::from(&p).downloaded, staged);

    // Back up: Retry fetches only the rest.
    let before = sink.received();
    srv.set_behavior(|_| Action::Serve);
    st.install(e, |_| {}).await.unwrap();
    assert_eq!(std::fs::read(st.path("m").unwrap()).unwrap(), bytes);
    assert_eq!(sink.received() - before, SIZE as u64 - staged + 1);
}

/// Start an install whose parts stall after `per_part` bytes; returns once they are committed.
async fn start_stalled_install(
    st: &Arc<ModelStore>,
    srv: &TestServer,
    e: &nugget_models::ModelEntry,
    per_part: usize,
) -> (
    Log,
    tokio::task::JoinHandle<nugget_core::Result<nugget_models::InstalledModel>>,
) {
    srv.set_behavior(move |r| match r.range {
        Some((s, Some(e))) if e > s => Action::StallAfter(per_part),
        _ => Action::Serve,
    });
    let (log, cb) = progress_log();
    let (st2, e2) = (st.clone(), e.clone());
    let task = tokio::spawn(async move { st2.install(&e2, cb).await });
    // 4 parts; commits happen every 512 bytes.
    let want = 4 * (per_part as u64 / 512) * 512;
    let l = log.clone();
    wait_for("staged bytes", || {
        l.lock()
            .unwrap()
            .iter()
            .any(|p| p.phase == Phase::Downloading && p.done >= want)
    })
    .await;
    (log, task)
}

#[tokio::test]
async fn cancel_stops_quickly_keeps_bytes_and_resume_fetches_only_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let sink = Arc::new(VecSink::default());
    // A stall timeout far beyond the test: only the cancel can end the first install.
    let st = Arc::new(
        store(tmp.path(), sink.clone()).with_download_options(DownloadOptions {
            stall_timeout: Duration::from_secs(60),
            persist_every: u64::MAX,
            ..fast_opts()
        }),
    );
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 7);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    let e = &m.models[0];

    assert!(!st.cancel_install(e), "nothing to cancel yet");
    let (log, task) = start_stalled_install(&st, &srv, e, 4096).await;
    let t0 = Instant::now();
    assert!(st.cancel_install(e));
    let err = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancel ends the install promptly")
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert!(t0.elapsed() < Duration::from_secs(2));

    // Terminal event: failed / cancelled, with the staged byte count.
    let p = last(&log);
    assert_eq!(
        (p.phase, p.reason),
        (Phase::Failed, Some(FailReason::Cancelled))
    );
    let staged = st.staged_bytes(e);
    assert_eq!(staged, 4 * 4096, "every committed byte is checkpointed");
    assert_eq!(p.done, staged);
    assert_eq!(
        ModelProgress::from(&p).reason,
        Some(ModelFailReason::Cancelled)
    );
    assert!(st.path("m").is_err());

    // Resume: each part's request starts where it stopped; only the rest is fetched.
    srv.clear_requests();
    srv.set_behavior(|_| Action::Serve);
    let before = sink.received();
    st.install(e, |_| {}).await.unwrap();
    assert_eq!(std::fs::read(st.path("m").unwrap()).unwrap(), bytes);
    assert_eq!(sink.received() - before, SIZE as u64 - staged + 1);
    let starts: HashSet<u64> = srv
        .requests()
        .iter()
        .filter_map(|r| r.range)
        .filter(|&(s, e)| !(s == 0 && e == Some(0)))
        .map(|(s, _)| s)
        .collect();
    assert_eq!(
        starts,
        HashSet::from([4096, 16384 + 4096, 32768 + 4096, 49152 + 4096])
    );
}

#[tokio::test]
async fn dropped_install_still_checkpoints_its_bytes() {
    // The old cancel path (dropping the future): state.json was only written every
    // `persist_every` bytes, so a resume restarted from the last checkpoint (byte 0 here).
    let tmp = tempfile::tempdir().unwrap();
    let st = Arc::new(
        store(tmp.path(), Arc::default()).with_download_options(DownloadOptions {
            stall_timeout: Duration::from_secs(60),
            persist_every: u64::MAX,
            ..fast_opts()
        }),
    );
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 8);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    let e = &m.models[0];

    let (_log, task) = start_stalled_install(&st, &srv, e, 2048).await;
    task.abort();
    let _ = task.await;
    wait_for("checkpoint after drop", || st.staged_bytes(e) == 4 * 2048).await;
}

#[tokio::test]
async fn resume_against_a_server_that_ignores_range_restarts_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let st = Arc::new(
        store(tmp.path(), Arc::default()).with_download_options(DownloadOptions {
            stall_timeout: Duration::from_secs(60),
            ..fast_opts()
        }),
    );
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 9);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    let e = &m.models[0];

    let (_log, task) = start_stalled_install(&st, &srv, e, 2048).await;
    st.cancel_install(e);
    let _ = task.await;
    assert!(st.staged_bytes(e) > 0);

    // The server now answers 200 with the whole file: start over from byte 0, still verified.
    srv.set_behavior(|_| Action::IgnoreRange);
    st.install(e, |_| {}).await.unwrap();
    assert_eq!(std::fs::read(st.path("m").unwrap()).unwrap(), bytes);
}

#[tokio::test]
async fn resumed_bytes_are_hash_verified() {
    // Staged bytes that were corrupted on disk are caught by the whole-file SHA-256 after resume.
    let tmp = tempfile::tempdir().unwrap();
    let st = Arc::new(
        store(tmp.path(), Arc::default()).with_download_options(DownloadOptions {
            stall_timeout: Duration::from_secs(60),
            ..fast_opts()
        }),
    );
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 10);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    let e = &m.models[0];

    let (_log, task) = start_stalled_install(&st, &srv, e, 2048).await;
    st.cancel_install(e);
    let _ = task.await;
    let part = st.staging_data_path(e);
    let mut staged = std::fs::read(&part).unwrap();
    staged[10] ^= 0xff;
    std::fs::write(&part, staged).unwrap();

    srv.set_behavior(|_| Action::Serve);
    let (log, cb) = progress_log();
    let err = st.install(e, cb).await.unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert_eq!(last(&log).reason, Some(FailReason::Integrity));
    assert_eq!(st.staged_bytes(e), 0, "bad staging is discarded");
    // The next attempt starts clean and succeeds.
    st.install(e, |_| {}).await.unwrap();
    assert_eq!(std::fs::read(st.path("m").unwrap()).unwrap(), bytes);
}

#[tokio::test]
async fn disk_full_reads_as_disk_full() {
    let tmp = tempfile::tempdir().unwrap();
    let st =
        store(tmp.path(), Arc::default()).with_disk_space(Arc::new(common::FakeDisk(Some(1024))));
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 11);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    let (log, cb) = progress_log();
    st.install(&m.models[0], cb).await.unwrap_err();
    let p = last(&log);
    assert_eq!(p.reason, Some(FailReason::DiskFull));
    assert!(p.error.unwrap().contains("disk space"));
}
