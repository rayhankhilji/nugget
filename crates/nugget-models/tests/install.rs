mod common;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Action, FakeDisk, TestServer, VecSink, data, entry, signed, store};
use nugget_core::Error;
use nugget_models::{EGRESS_FEATURE, Phase, Progress};

const SIZE: usize = 64 * 1024;

fn progress_log() -> (
    Arc<Mutex<Vec<Progress>>>,
    impl Fn(Progress) + Send + Sync + 'static,
) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    (log, move |p| l.lock().unwrap().push(p))
}

/// Range starts of real part requests (excludes the 1-byte probe).
fn part_starts(srv: &TestServer) -> Vec<u64> {
    srv.requests()
        .iter()
        .filter_map(|r| r.range)
        .filter(|&(s, e)| !(s == 0 && e == Some(0)))
        .map(|(s, _)| s)
        .collect()
}

#[tokio::test]
async fn parallel_parts_download_verify_and_install() {
    let tmp = tempfile::tempdir().unwrap();
    let sink = Arc::new(VecSink::default());
    let st = store(tmp.path(), sink.clone());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 1);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    srv.set_delay(Duration::from_millis(150));

    let (log, cb) = progress_log();
    let installed = st.install(&m.models[0], cb).await.unwrap();

    assert_eq!(installed.sha256, m.models[0].sha256);
    let path = st.path("m").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(path.ends_with(
        format!("blobs/sha256/{}", installed.sha256).replace('/', std::path::MAIN_SEPARATOR_STR)
    ));

    // Four distinct parts, fetched concurrently.
    let starts: HashSet<u64> = part_starts(&srv).into_iter().collect();
    assert_eq!(starts, HashSet::from([0, 16384, 32768, 49152]));
    assert!(
        srv.max_inflight() >= 2,
        "max in flight = {}",
        srv.max_inflight()
    );

    // Staging cleaned up; egress recorded against the serving host.
    assert!(
        !tmp.path()
            .join("models/staging")
            .join(&installed.sha256)
            .exists()
    );
    let events = sink.0.lock().unwrap().clone();
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .all(|e| e.host == "127.0.0.1" && e.feature == EGRESS_FEATURE)
    );
    assert_eq!(
        sink.received(),
        SIZE as u64 + 1,
        "body bytes + 1-byte probe"
    );
    assert!(events.iter().map(|e| e.bytes_sent).sum::<u64>() > 0);

    // Progress: downloading is monotonic, verifying follows, installed is last.
    let log = log.lock().unwrap();
    let dl: Vec<u64> = log
        .iter()
        .filter(|p| p.phase == Phase::Downloading)
        .map(|p| p.done)
        .collect();
    assert!(dl.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(*dl.last().unwrap(), SIZE as u64);
    assert!(log.iter().any(|p| p.phase == Phase::Verifying));
    let last = log.last().unwrap();
    assert_eq!(
        (last.phase, last.done, last.total),
        (Phase::Installed, SIZE as u64, SIZE as u64)
    );
    let ipc: nugget_core::ipc::ModelProgress = last.into();
    assert_eq!(ipc.phase, nugget_core::ipc::ModelPhase::Installed);
}

#[tokio::test]
async fn resumes_within_a_part_after_connection_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let sink = Arc::new(VecSink::default());
    let st = store(tmp.path(), sink.clone());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 2);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);

    // The first request for each part dies halfway through the body.
    let seen = Mutex::new(HashSet::new());
    srv.set_behavior(move |r| match r.range {
        Some((s, Some(e))) if e > s && seen.lock().unwrap().insert(s) => {
            let len = e - s + 1;
            Action::DropAfter((len / 2) as usize)
        }
        _ => Action::Serve,
    });

    st.install(&m.models[0], |_| {}).await.unwrap();
    assert_eq!(std::fs::read(st.path("m").unwrap()).unwrap(), bytes);

    // Follow-up requests start mid-part, where the dropped transfer stopped.
    let starts = part_starts(&srv);
    for mid in [8192u64, 16384 + 8192, 32768 + 8192, 49152 + 8192] {
        assert!(starts.contains(&mid), "no resume at {mid}: {starts:?}");
    }
    // Nothing was downloaded twice.
    assert_eq!(sink.received(), SIZE as u64 + 1);
}

#[tokio::test]
async fn resumes_across_install_calls_from_staged_state() {
    let tmp = tempfile::tempdir().unwrap();
    let sink = Arc::new(VecSink::default());
    let st = store(tmp.path(), sink.clone());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 3);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    let e = &m.models[0];

    // Session 1: the third part is short-changed and then permanently unavailable.
    let third = 32768u64;
    let hits = Mutex::new(0);
    srv.set_behavior(move |r| match r.range {
        Some((s, _)) if (third..third + 16384).contains(&s) => {
            let mut h = hits.lock().unwrap();
            *h += 1;
            if *h == 1 {
                Action::Short(1000)
            } else {
                Action::Status(404)
            }
        }
        _ => Action::Serve,
    });
    let err = st.install(e, |_| {}).await.unwrap_err();
    assert!(err.to_string().contains("404"), "{err}");
    let staged = st.staged_bytes(e);
    assert!(staged >= 1000 && staged < SIZE as u64, "staged {staged}");
    assert!(st.path("m").is_err());

    // Session 2: a fresh store on the same root picks up exactly where session 1 stopped.
    let before = sink.received();
    srv.set_behavior(|_| Action::Serve);
    srv.clear_requests();
    let st2 = store(tmp.path(), sink.clone());
    st2.install(e, |_| {}).await.unwrap();
    assert_eq!(std::fs::read(st2.path("m").unwrap()).unwrap(), bytes);
    assert_eq!(sink.received() - before, SIZE as u64 - staged + 1);
    assert!(part_starts(&srv).contains(&(third + 1000)));
}

#[tokio::test]
async fn hash_mismatch_wipes_staging_and_installs_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 4);
    let url = srv.add("m", bytes.clone());
    // Manifest claims different content of the same size.
    let m = signed(vec![entry("m", &url, &data(SIZE, 5))]);
    let e = &m.models[0];

    let (log, cb) = progress_log();
    let err = st.install(e, cb).await.unwrap_err();
    assert!(
        matches!(&err, Error::Integrity(m) if m.contains("SHA-256 mismatch")),
        "{err}"
    );
    assert!(!tmp.path().join("models/staging").join(&e.sha256).exists());
    assert!(
        !tmp.path()
            .join("models/blobs/sha256")
            .join(&e.sha256)
            .exists()
    );
    assert_eq!(st.staged_bytes(e), 0);
    assert!(st.slot("m").unwrap().is_none());
    let last = log.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.phase, Phase::Failed);
    assert!(last.error.unwrap().contains("mismatch"));
}

#[tokio::test]
async fn preflight_fails_closed_before_any_network() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 6);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);

    // Unreadable free space is an error, not a pass.
    let st = store(tmp.path(), Arc::default()).with_disk_space(Arc::new(FakeDisk(None)));
    let err = st.install(&m.models[0], |_| {}).await.unwrap_err();
    assert!(
        matches!(&err, Error::Storage(m) if m.contains("cannot determine")),
        "{err}"
    );

    // Not enough room: size + 10% + 1 GiB is required.
    let need = nugget_models::required_bytes(SIZE as u64, 0);
    let st = store(tmp.path(), Arc::default()).with_disk_space(Arc::new(FakeDisk(Some(need - 1))));
    let err = st.install(&m.models[0], |_| {}).await.unwrap_err();
    assert!(
        matches!(err, Error::InsufficientSpace { needed, available } if needed == need && available == need - 1),
        "{err}"
    );

    assert!(
        srv.requests().is_empty(),
        "no request may precede the preflight"
    );

    let st = store(tmp.path(), Arc::default()).with_disk_space(Arc::new(FakeDisk(Some(need))));
    st.install(&m.models[0], |_| {}).await.unwrap();
}

#[tokio::test]
async fn retries_transient_errors_and_gives_up_on_fatal_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 7);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);

    // Every part fails twice with 503 before succeeding (max_retries = 3).
    let fails = Mutex::new(std::collections::HashMap::<u64, u32>::new());
    srv.set_behavior(move |r| match r.range {
        Some((s, Some(e))) if e > s => {
            let mut f = fails.lock().unwrap();
            let n = f.entry(s).or_default();
            *n += 1;
            if *n <= 2 {
                Action::Status(503)
            } else {
                Action::Serve
            }
        }
        _ => Action::Serve,
    });
    st.install(&m.models[0], |_| {}).await.unwrap();
    assert_eq!(part_starts(&srv).len(), 12);

    // A 403 is not retried.
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    srv.clear_requests();
    srv.set_behavior(|_| Action::Status(403));
    let err = st.install(&m.models[0], |_| {}).await.unwrap_err();
    assert!(err.to_string().contains("403"), "{err}");
    assert_eq!(srv.requests().len(), 1);
}

#[tokio::test]
async fn network_and_rate_limit_failures_use_their_error_variants() {
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 9);
    let url = srv.add("m", bytes.clone());

    // Persistent 503: retried, then reported as a network failure.
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let m = signed(vec![entry("m", &url, &bytes)]);
    srv.set_behavior(|_| Action::Status(503));
    let err = st.install(&m.models[0], |_| {}).await.unwrap_err();
    assert!(
        matches!(&err, Error::Network(m) if m.contains("503")),
        "{err}"
    );

    // Persistent 429: retried, then reported as rate limited.
    srv.set_behavior(|_| Action::Status(429));
    let err = st.install(&m.models[0], |_| {}).await.unwrap_err();
    assert!(matches!(err, Error::RateLimited { .. }), "{err}");

    // Nothing listening: a connection failure is a network error.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let dead = url.replace(&srv.base, &format!("http://127.0.0.1:{port}"));
    let m = signed(vec![entry("m", &dead, &bytes)]);
    let err = st.install(&m.models[0], |_| {}).await.unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err}");
}

#[tokio::test]
async fn falls_back_to_mirror_and_follows_redirects() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 8);
    // Primary serves a file of the wrong size; the mirror redirects to the real bytes.
    let primary = srv.add("wrong", data(SIZE + 1, 8));
    let real = srv.add("real", bytes.clone());
    let mirror = format!("{}/mirror/resolve/{}/m.gguf", srv.base, common::REV);
    let target = real.clone();
    srv.set_behavior(move |r| {
        if r.path.starts_with("/mirror/") {
            Action::Redirect(target.clone())
        } else {
            Action::Serve
        }
    });
    let mut e = entry("m", &primary, &bytes);
    e.mirrors = vec![mirror];
    let m = signed(vec![e]);
    st.install(&m.models[0], |_| {}).await.unwrap();
    assert_eq!(std::fs::read(st.path("m").unwrap()).unwrap(), bytes);
    assert!(
        srv.requests()
            .iter()
            .any(|r| r.path.starts_with("/real/") && r.range.is_some())
    );
}

#[tokio::test]
async fn server_without_range_support_downloads_sequentially() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 9);
    let url = srv.add("m", bytes.clone());
    srv.set_behavior(|_| Action::IgnoreRange);
    let m = signed(vec![entry("m", &url, &bytes)]);
    st.install(&m.models[0], |_| {}).await.unwrap();
    assert_eq!(std::fs::read(st.path("m").unwrap()).unwrap(), bytes);
    assert_eq!(
        srv.requests().len(),
        1,
        "the probe response is the download"
    );
}

#[tokio::test]
async fn rollback_swaps_active_and_previous() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let srv = TestServer::start().await;
    let (a, b) = (data(4096, 10), data(5000, 11));
    let (ua, ub) = (srv.add("a", a.clone()), srv.add("b", b.clone()));
    let m = signed(vec![entry("x", &ua, &a), entry("y", &ub, &b)]);
    let (v1, mut v2) = (m.models[0].clone(), m.models[1].clone());
    v2.id = "x".into();

    let err = st.rollback("x").await.unwrap_err();
    assert!(matches!(err, Error::NotFound(_)));

    st.install(&v1, |_| {}).await.unwrap();
    assert!(matches!(
        st.rollback("x").await.unwrap_err(),
        Error::NotFound(_)
    ));
    assert!(st.needs_update(&v2).unwrap());
    st.install(&v2, |_| {}).await.unwrap();
    assert!(!st.needs_update(&v2).unwrap());
    let slot = st.slot("x").unwrap().unwrap();
    assert_eq!(slot.active.sha256, v2.sha256);
    assert_eq!(slot.previous.unwrap().sha256, v1.sha256);
    assert_eq!(std::fs::read(st.path("x").unwrap()).unwrap(), b);

    let now = st.rollback("x").await.unwrap();
    assert_eq!(now.sha256, v1.sha256);
    assert_eq!(std::fs::read(st.path("x").unwrap()).unwrap(), a);
    // Rolling back again rolls forward.
    st.rollback("x").await.unwrap();
    assert_eq!(std::fs::read(st.path("x").unwrap()).unwrap(), b);

    // A previous version whose blob vanished cannot be activated.
    std::fs::remove_file(tmp.path().join("models/blobs/sha256").join(&v1.sha256)).unwrap();
    assert!(matches!(
        st.rollback("x").await.unwrap_err(),
        Error::Integrity(_)
    ));
    assert_eq!(std::fs::read(st.path("x").unwrap()).unwrap(), b);
}

#[tokio::test]
async fn gc_removes_only_unreferenced_blobs() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let srv = TestServer::start().await;
    let blobs: Vec<Vec<u8>> = (0..3)
        .map(|i| data(3000 + i * 100, 20 + i as u64))
        .collect();
    let entries: Vec<_> = blobs
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let mut e = entry("m", &srv.add(&format!("v{i}"), b.clone()), b);
            e.id = "m".into();
            e
        })
        .collect();
    let other = data(2000, 99);
    let keep = entry("other", &srv.add("other", other.clone()), &other);

    assert_eq!(st.gc().await.unwrap(), Default::default());
    st.install(&keep, |_| {}).await.unwrap();
    for e in &entries {
        st.install(e, |_| {}).await.unwrap();
    }
    // v0 is neither active (v2) nor previous (v1).
    let report = st.gc().await.unwrap();
    assert_eq!(report.removed, vec![entries[0].sha256.clone()]);
    assert_eq!(report.freed_bytes, blobs[0].len() as u64);
    assert_eq!(std::fs::read(st.path("m").unwrap()).unwrap(), blobs[2]);
    assert_eq!(std::fs::read(st.path("other").unwrap()).unwrap(), other);
    st.rollback("m").await.unwrap();
    assert_eq!(std::fs::read(st.path("m").unwrap()).unwrap(), blobs[1]);
    assert!(st.gc().await.unwrap().removed.is_empty());
}

#[tokio::test]
async fn verify_detects_corruption_and_reinstall_reuses_good_blob() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 30);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    let e = &m.models[0];

    assert!(matches!(
        st.verify("m").await.unwrap_err(),
        Error::NotFound(_)
    ));
    st.install(e, |_| {}).await.unwrap();
    st.verify("m").await.unwrap();

    // Reinstalling identical bytes re-hashes locally and makes no part requests.
    srv.clear_requests();
    st.install(e, |_| {}).await.unwrap();
    assert!(srv.requests().is_empty());

    // Same-size corruption: path() (cheap) passes, verify() (full hash) fails.
    let p = st.path("m").unwrap();
    let mut b = std::fs::read(&p).unwrap();
    b[100] ^= 0xff;
    std::fs::write(&p, &b).unwrap();
    assert!(st.path("m").is_ok());
    assert!(matches!(
        st.verify("m").await.unwrap_err(),
        Error::Integrity(_)
    ));

    // A reinstall notices the bad blob and downloads it again.
    st.install(e, |_| {}).await.unwrap();
    st.verify("m").await.unwrap();

    // Truncation is caught by the cheap check.
    std::fs::write(&p, b"short").unwrap();
    assert!(matches!(st.path("m").unwrap_err(), Error::Integrity(_)));
}

#[tokio::test]
async fn refuses_entries_for_newer_apps_and_unvalidated_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path(), Arc::default());
    let srv = TestServer::start().await;
    let bytes = data(1000, 40);
    let url = srv.add("m", bytes.clone());
    let mut e = entry("m", &url, &bytes);
    e.min_app_version = "99.0.0".into();
    let m = signed(vec![e]);
    let err = st.install(&m.models[0], |_| {}).await.unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err}");

    // A hand-built entry pointing at a moving ref never reaches the network.
    let mut bad = entry("m", &url, &bytes);
    bad.revision = "main".into();
    assert!(matches!(
        st.install(&bad, |_| {}).await.unwrap_err(),
        Error::Invalid(_)
    ));
    assert!(srv.requests().is_empty());
}

#[tokio::test]
async fn cancelled_install_keeps_staged_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    // Retry forever so only the cancellation ends the first attempt.
    let st =
        store(tmp.path(), Arc::default()).with_download_options(nugget_models::DownloadOptions {
            max_retries: u32::MAX,
            ..common::fast_opts()
        });
    let srv = TestServer::start().await;
    let bytes = data(SIZE, 50);
    let url = srv.add("m", bytes.clone());
    let m = signed(vec![entry("m", &url, &bytes)]);
    let e = &m.models[0];

    // Part 0 is served; the other parts keep failing until we give up waiting.
    srv.set_behavior(|r| match r.range {
        Some((s, _)) if s >= 16384 => Action::Status(503),
        _ => Action::Serve,
    });
    let res = tokio::time::timeout(Duration::from_millis(300), st.install(e, |_| {})).await;
    assert!(
        res.is_err(),
        "install should still be running when cancelled"
    );
    assert_eq!(st.staged_bytes(e), 16384);

    srv.set_behavior(|_| Action::Serve);
    srv.clear_requests();
    st.install(e, |_| {}).await.unwrap();
    assert!(
        !part_starts(&srv).contains(&0),
        "part 0 must not be fetched again"
    );
    assert_eq!(std::fs::read(st.path("m").unwrap()).unwrap(), bytes);
    // Nothing left to discard; discarding is idempotent.
    st.discard_staging(e).unwrap();
}
