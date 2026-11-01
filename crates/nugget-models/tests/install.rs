//! Model install: parallel resumable download, verification, failure classes (fast,
//! plain-language, with staged bytes), cancellation, and the content-addressed store.

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use nugget_core::Error;
use nugget_core::ipc::{ModelFailReason, ModelPhase, ModelProgress};
use nugget_models::{DownloadOptions, EGRESS_FEATURE, FailReason, ModelEntry, ModelStore, Phase};

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

/// Long stall timeout: only a cancel (or the test) ends a stalled install.
fn patient() -> DownloadOptions {
    DownloadOptions {
        stall_timeout: Duration::from_secs(60),
        persist_every: u64::MAX,
        ..fast_opts()
    }
}

#[tokio::test]
async fn parallel_parts_download_verify_and_install() {
    let fx = fixture(1).await;
    let st = fx.store();
    fx.srv.set_delay(Duration::from_millis(150));
    let (log, cb) = progress_log();
    let installed = st.install(&fx.e, cb).await.unwrap();
    assert_eq!(installed.sha256, fx.e.sha256);
    fx.assert_installed(&st);
    let blob =
        format!("blobs/sha256/{}", installed.sha256).replace('/', std::path::MAIN_SEPARATOR_STR);
    assert!(st.path("m").unwrap().ends_with(blob));

    // Four distinct parts, fetched concurrently.
    let starts: HashSet<u64> = part_starts(&fx.srv).into_iter().collect();
    assert_eq!(starts, HashSet::from([0, 16384, 32768, 49152]));
    assert!(
        fx.srv.max_inflight() >= 2,
        "max in flight = {}",
        fx.srv.max_inflight()
    );

    // Staging cleaned up; egress recorded against the serving host.
    assert!(
        !fx.tmp
            .path()
            .join("models/staging")
            .join(&installed.sha256)
            .exists()
    );
    let events = fx.sink.0.lock().unwrap().clone();
    assert!(
        !events.is_empty()
            && events
                .iter()
                .all(|e| e.host == "127.0.0.1" && e.feature == EGRESS_FEATURE)
    );
    assert_eq!(
        fx.sink.received(),
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
    assert_eq!(ModelProgress::from(last).phase, ModelPhase::Installed);
}

#[tokio::test]
async fn transient_errors_are_retried_and_drops_resume_mid_part() {
    let fx = fixture(2).await;
    // Each part: first a 503, then the body dies halfway, then it is served.
    let tries = Mutex::new(HashMap::<u64, u32>::new());
    fx.srv.set_behavior(move |r| match r.range {
        Some((s, Some(e))) if e > s => {
            let mut t = tries.lock().unwrap();
            let n = t.entry(e).or_default();
            *n += 1;
            match n {
                1 => Action::Status(503),
                2 => Action::DropAfter(((e - s + 1) / 2) as usize),
                _ => Action::Serve,
            }
        }
        _ => Action::Serve,
    });
    let st = fx.store();
    st.install(&fx.e, |_| {}).await.unwrap();
    fx.assert_installed(&st);
    // Follow-up requests start mid-part, where the dropped transfer stopped.
    let starts = part_starts(&fx.srv);
    for mid in [8192u64, 16384 + 8192, 32768 + 8192, 49152 + 8192] {
        assert!(starts.contains(&mid), "no resume at {mid}: {starts:?}");
    }
    assert_eq!(
        fx.sink.received(),
        SIZE as u64 + 1,
        "nothing downloaded twice"
    );
}

/// Failures are reported quickly, with a plain message, the right error variant and reason, and
/// only transient ones are retried.
#[tokio::test]
async fn failures_are_classified_and_reported_quickly() {
    type Check = fn(&Error) -> bool;
    let cases: [(Action, FailReason, usize, Check); 6] = [
        (
            Action::Status(500),
            FailReason::HttpError(500),
            4,
            |e| matches!(e, Error::Network(m) if m.contains("500")),
        ),
        (Action::Status(429), FailReason::HttpError(429), 4, |e| {
            matches!(e, Error::RateLimited { .. })
        }),
        (Action::Status(404), FailReason::HttpError(404), 1, |e| {
            e.to_string().contains("404")
        }),
        (Action::Status(403), FailReason::HttpError(403), 1, |e| {
            matches!(e, Error::Other(_))
        }),
        // KK1 review: 511 Network Authentication Required is the standard captive-portal answer.
        (Action::Status(511), FailReason::CaptivePortal, 1, |_| true),
        // A sign-in page served as the file is never staged.
        (Action::Html, FailReason::CaptivePortal, 1, |_| true),
    ];
    for (action, reason, max_requests, check) in cases {
        let fx = fixture(3).await;
        let st = fx.store();
        fx.srv.set_behavior(move |_| action.clone());
        let (log, cb) = progress_log();
        let t0 = Instant::now();
        let err = st.install(&fx.e, cb).await.unwrap_err();
        assert!(
            t0.elapsed() < Duration::from_secs(3),
            "{reason:?}: {:?}",
            t0.elapsed()
        );
        assert!(check(&err), "{reason:?}: {err}");
        assert!(
            fx.srv.requests().len() <= max_requests,
            "{reason:?}: {}",
            fx.srv.requests().len()
        );
        let p = last(&log);
        assert_eq!(
            (p.phase, p.reason, p.done),
            (Phase::Failed, Some(reason), 0)
        );
        assert_plain(p.error.as_deref().unwrap());
        assert_eq!(st.staged_bytes(&fx.e), 0);
        assert!(
            !fx.sink.0.lock().unwrap().is_empty(),
            "the request is in the egress ledger"
        );
        if let FailReason::HttpError(code) = reason {
            let ipc = ModelProgress::from(&p);
            assert_eq!(
                (ipc.reason, ipc.http_status),
                (Some(ModelFailReason::HttpError), Some(code))
            );
        }
    }

    // Offline, with production retry settings: reported within seconds, not after a long ladder.
    let fx = fixture(4).await;
    let st = fx.store().with_download_options(DownloadOptions::default());
    let mut e = fx.e.clone();
    e.url = dead_url(&fx.srv, &e.url);
    let (log, cb) = progress_log();
    let t0 = Instant::now();
    let err = st.install(&e, cb).await.unwrap_err();
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "took {:?}",
        t0.elapsed()
    );
    assert!(matches!(err, Error::Network(_)), "{err}");
    let p = last(&log);
    assert_eq!(p.reason, Some(FailReason::Offline));
    assert!(p.error.as_deref().unwrap().contains("offline"));
    assert_plain(p.error.as_deref().unwrap());
    assert_eq!(
        ModelProgress::from(&p).reason,
        Some(ModelFailReason::Offline)
    );
}

#[tokio::test]
async fn stalls_fail_with_staged_bytes_but_brief_ones_resume() {
    for brief in [false, true] {
        let fx = fixture(5).await;
        let st = fx.store_with(DownloadOptions {
            stall_timeout: Duration::from_millis(300),
            ..fast_opts()
        });
        // Each part's first request goes silent after 2 KiB; later ones are silent or served.
        let seen = Mutex::new(HashSet::new());
        fx.srv.set_behavior(move |r| match r.range {
            Some((s, Some(e))) if e > s && seen.lock().unwrap().insert(e) => {
                Action::StallAfter(2048)
            }
            Some((s, Some(e))) if e > s && !brief => Action::StallAfter(0),
            _ => Action::Serve,
        });
        let (log, cb) = progress_log();
        let t0 = Instant::now();
        let res = st.install(&fx.e, cb).await;
        if brief {
            // KK1 review: a brief silence (Wi-Fi roaming) is not a failure.
            assert_eq!(res.unwrap().size, SIZE as u64);
            continue;
        }
        res.unwrap_err();
        assert!(t0.elapsed() < Duration::from_secs(4), "{:?}", t0.elapsed());
        let p = last(&log);
        assert_eq!(p.reason, Some(FailReason::Stalled));
        assert_plain(p.error.as_deref().unwrap());
        assert!(p.done >= 2048, "staged {}", p.done);
        assert_eq!(p.done, st.staged_bytes(&fx.e));
    }
}

#[tokio::test]
async fn server_dying_midway_reports_the_staged_bytes_and_retry_resumes() {
    let fx = fixture(6).await;
    let st = fx.store();
    // Each part's first request delivers half, then the server is gone for good.
    let seen = Mutex::new(HashSet::new());
    fx.srv.set_behavior(move |r| match r.range {
        // Keyed by the part's end: a resumed request starts elsewhere but ends there too.
        Some((s, Some(e))) if e > s && seen.lock().unwrap().insert(e) => {
            Action::DropAfter(((e - s + 1) >> 1) as usize)
        }
        Some((s, Some(e))) if e > s => Action::Close,
        _ => Action::Serve,
    });
    let (log, cb) = progress_log();
    let t0 = Instant::now();
    st.install(&fx.e, cb).await.unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    let p = last(&log);
    assert_eq!(
        (p.phase, p.reason),
        (Phase::Failed, Some(FailReason::Interrupted))
    );
    assert_plain(p.error.as_deref().unwrap());
    let staged = st.staged_bytes(&fx.e);
    assert_eq!(p.done, staged);
    assert!(staged >= SIZE as u64 / 2 - 4 * 512, "staged {staged}");
    assert_eq!(ModelProgress::from(&p).downloaded, staged);

    // Back up: a fresh store on the same root fetches only the rest.
    let before = fx.sink.received();
    fx.srv.set_behavior(|_| Action::Serve);
    let st = fx.store();
    st.install(&fx.e, |_| {}).await.unwrap();
    fx.assert_installed(&st);
    assert_eq!(fx.sink.received() - before, SIZE as u64 - staged + 1);
}

/// Start an install whose parts stall after `per_part` bytes; returns once they are committed.
async fn start_stalled_install(
    st: &Arc<ModelStore>,
    srv: &TestServer,
    e: &ModelEntry,
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
    let fx = fixture(7).await;
    let st = fx.store_with(patient());
    assert!(!st.cancel_install(&fx.e), "nothing to cancel yet");
    let (log, task) = start_stalled_install(&st, &fx.srv, &fx.e, 4096).await;
    let t0 = Instant::now();
    assert!(st.cancel_install(&fx.e));
    let err = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancel ends the install promptly")
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert!(t0.elapsed() < Duration::from_secs(2));
    let p = last(&log);
    assert_eq!(
        (p.phase, p.reason),
        (Phase::Failed, Some(FailReason::Cancelled))
    );
    let staged = st.staged_bytes(&fx.e);
    assert_eq!(staged, 4 * 4096, "every committed byte is checkpointed");
    assert_eq!(p.done, staged);
    assert_eq!(
        ModelProgress::from(&p).reason,
        Some(ModelFailReason::Cancelled)
    );
    assert!(st.path("m").is_err());

    // Resume: each part's request starts where it stopped; only the rest is fetched.
    fx.srv.clear_requests();
    fx.srv.set_behavior(|_| Action::Serve);
    let before = fx.sink.received();
    st.install(&fx.e, |_| {}).await.unwrap();
    fx.assert_installed(&st);
    assert_eq!(fx.sink.received() - before, SIZE as u64 - staged + 1);
    let starts: HashSet<u64> = part_starts(&fx.srv).into_iter().collect();
    assert_eq!(
        starts,
        HashSet::from([4096, 16384 + 4096, 32768 + 4096, 49152 + 4096])
    );
    st.discard_staging(&fx.e).unwrap();
}

/// Dropping the install future (not cancelling it) still checkpoints every committed byte.
#[tokio::test]
async fn dropped_install_still_checkpoints_its_bytes() {
    let fx = fixture(8).await;
    let st = fx.store_with(patient());
    let (_log, task) = start_stalled_install(&st, &fx.srv, &fx.e, 2048).await;
    task.abort();
    let _ = task.await;
    wait_for("checkpoint after drop", || {
        st.staged_bytes(&fx.e) == 4 * 2048
    })
    .await;
}

#[tokio::test]
async fn servers_ignoring_range_are_downloaded_whole() {
    let fx = fixture(9).await;
    let st = fx.store_with(patient());
    let (_log, task) = start_stalled_install(&st, &fx.srv, &fx.e, 2048).await;
    st.cancel_install(&fx.e);
    let _ = task.await;
    assert!(st.staged_bytes(&fx.e) > 0);
    // The server now answers 200 with the whole file: start over from byte 0, still verified.
    fx.srv.clear_requests();
    fx.srv.set_behavior(|_| Action::IgnoreRange);
    st.install(&fx.e, |_| {}).await.unwrap();
    fx.assert_installed(&st);
    assert_eq!(
        fx.srv.requests().len(),
        1,
        "the probe response is the download"
    );
}

#[tokio::test]
async fn bad_bytes_are_discarded_and_never_installed() {
    // Corrupted staged bytes are caught by the whole-file SHA-256 after a resume.
    let fx = fixture(10).await;
    let st = fx.store_with(patient());
    let (_log, task) = start_stalled_install(&st, &fx.srv, &fx.e, 2048).await;
    st.cancel_install(&fx.e);
    let _ = task.await;
    let part = st.staging_data_path(&fx.e);
    let mut staged = std::fs::read(&part).unwrap();
    staged[10] ^= 0xff;
    std::fs::write(&part, staged).unwrap();
    fx.srv.set_behavior(|_| Action::Serve);
    let (log, cb) = progress_log();
    assert!(matches!(
        st.install(&fx.e, cb).await.unwrap_err(),
        Error::Integrity(_)
    ));
    assert_eq!(last(&log).reason, Some(FailReason::Integrity));
    assert_eq!(st.staged_bytes(&fx.e), 0, "bad staging is discarded");
    st.install(&fx.e, |_| {}).await.unwrap();
    fx.assert_installed(&st);

    // A manifest claiming other content of the same size.
    let mut e = fx.e.clone();
    e.sha256 = sha256_hex(&data(SIZE, 11));
    let (log, cb) = progress_log();
    let err = st.install(&e, cb).await.unwrap_err();
    assert!(
        matches!(&err, Error::Integrity(m) if m.contains("SHA-256 mismatch")),
        "{err}"
    );
    assert!(
        !fx.tmp
            .path()
            .join("models/staging")
            .join(&e.sha256)
            .exists()
    );
    assert!(
        !fx.tmp
            .path()
            .join("models/blobs/sha256")
            .join(&e.sha256)
            .exists()
    );
    let last = last(&log);
    assert_eq!(
        (last.phase, last.reason, last.done),
        (Phase::Failed, Some(FailReason::Integrity), 0)
    );
    assert!(last.error.unwrap().contains("discarded"));
}

#[tokio::test]
async fn preflight_fails_closed_before_any_network() {
    let fx = fixture(12).await;
    let disk = |b| Arc::new(FakeDisk(b));
    // Unreadable free space is an error, not a pass.
    let err = fx
        .store()
        .with_disk_space(disk(None))
        .install(&fx.e, |_| {})
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Storage(m) if m.contains("cannot determine")),
        "{err}"
    );
    // Not enough room: size + 10% + 1 GiB is required.
    let need = nugget_models::required_bytes(SIZE as u64, 0);
    let (log, cb) = progress_log();
    let err = fx
        .store()
        .with_disk_space(disk(Some(need - 1)))
        .install(&fx.e, cb)
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::InsufficientSpace { needed, available } if needed == need && available == need - 1),
        "{err}"
    );
    let p = last(&log);
    assert_eq!(p.reason, Some(FailReason::DiskFull));
    assert!(p.error.unwrap().contains("disk space"));
    assert!(
        fx.srv.requests().is_empty(),
        "no request may precede the preflight"
    );
    fx.store()
        .with_disk_space(disk(Some(need)))
        .install(&fx.e, |_| {})
        .await
        .unwrap();
}

#[tokio::test]
async fn falls_back_to_mirror_and_follows_redirects() {
    let fx = fixture(13).await;
    // Primary serves a file of the wrong size; the mirror redirects to the real bytes.
    let primary = fx.srv.add("wrong", data(SIZE + 1, 8));
    let mirror = format!("{}/mirror/resolve/{}/m.gguf", fx.srv.base, REV);
    let target = fx.e.url.clone();
    fx.srv.set_behavior(move |r| {
        if r.path.starts_with("/mirror/") {
            Action::Redirect(target.clone())
        } else {
            Action::Serve
        }
    });
    let mut e = entry("m", &primary, &fx.bytes);
    e.mirrors = vec![mirror];
    let st = fx.store();
    st.install(&signed(vec![e]).models[0], |_| {})
        .await
        .unwrap();
    fx.assert_installed(&st);
    assert!(
        fx.srv
            .requests()
            .iter()
            .any(|r| r.path.starts_with("/m/") && r.range.is_some())
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
    let read = || std::fs::read(st.path("x").unwrap()).unwrap();

    assert!(matches!(
        st.rollback("x").await.unwrap_err(),
        Error::NotFound(_)
    ));
    st.install(&v1, |_| {}).await.unwrap();
    assert!(matches!(
        st.rollback("x").await.unwrap_err(),
        Error::NotFound(_)
    ));
    assert!(st.needs_update(&v2).unwrap());
    st.install(&v2, |_| {}).await.unwrap();
    assert!(!st.needs_update(&v2).unwrap());
    let slot = st.slot("x").unwrap().unwrap();
    assert_eq!(
        (&slot.active.sha256, &slot.previous.unwrap().sha256),
        (&v2.sha256, &v1.sha256)
    );
    assert_eq!(read(), b);

    assert_eq!(st.rollback("x").await.unwrap().sha256, v1.sha256);
    assert_eq!(read(), a);
    // Rolling back again rolls forward.
    st.rollback("x").await.unwrap();
    assert_eq!(read(), b);
    // A previous version whose blob vanished cannot be activated.
    std::fs::remove_file(tmp.path().join("models/blobs/sha256").join(&v1.sha256)).unwrap();
    assert!(matches!(
        st.rollback("x").await.unwrap_err(),
        Error::Integrity(_)
    ));
    assert_eq!(read(), b);
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
        .map(|(i, b)| entry("m", &srv.add(&format!("v{i}"), b.clone()), b))
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

/// gc runs while install re-verifies a blob that is present but not yet referenced. Install must
/// notice the blob is gone and fetch it again rather than activate a slot pointing at nothing.
#[tokio::test]
async fn gc_during_verify_of_existing_blob_does_not_activate_a_missing_blob() {
    let fx = fixture(61).await;
    let st = Arc::new(fx.store());
    let blob = st.root().join("blobs").join("sha256").join(&fx.e.sha256);
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, &fx.bytes).unwrap();

    let ran_gc = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (gc_store, flag, sha) = (st.clone(), ran_gc.clone(), fx.e.sha256.clone());
    let handle = tokio::runtime::Handle::current();
    let installed = st
        .install(&fx.e, move |p| {
            // Runs on the hashing thread, after the blob was read and before activation.
            if p.phase == Phase::Verifying
                && p.done == p.total
                && !flag.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                assert_eq!(
                    handle.block_on(gc_store.gc()).unwrap().removed,
                    vec![sha.clone()]
                );
            }
        })
        .await
        .unwrap();
    assert!(ran_gc.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(installed.sha256, fx.e.sha256);
    fx.assert_installed(&st);
    assert!(
        !fx.srv.requests().is_empty(),
        "the removed blob was downloaded again"
    );
}

#[tokio::test]
async fn verify_detects_corruption_and_reinstall_reuses_good_blob() {
    let fx = fixture(30).await;
    let st = fx.store();
    assert!(matches!(
        st.verify("m").await.unwrap_err(),
        Error::NotFound(_)
    ));
    st.install(&fx.e, |_| {}).await.unwrap();
    st.verify("m").await.unwrap();
    // Reinstalling identical bytes re-hashes locally and makes no requests.
    fx.srv.clear_requests();
    st.install(&fx.e, |_| {}).await.unwrap();
    assert!(fx.srv.requests().is_empty());

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
    st.install(&fx.e, |_| {}).await.unwrap();
    st.verify("m").await.unwrap();
    // Truncation is caught by the cheap check.
    std::fs::write(&p, b"short").unwrap();
    assert!(matches!(st.path("m").unwrap_err(), Error::Integrity(_)));
}

#[tokio::test]
async fn refuses_entries_for_newer_apps_and_unvalidated_entries() {
    let fx = fixture(40).await;
    let st = fx.store();
    let mut e = fx.e.clone();
    e.min_app_version = "99.0.0".into();
    let err = st
        .install(&signed(vec![e]).models[0], |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err}");
    // A hand-built entry pointing at a moving ref never reaches the network.
    let mut bad = fx.e.clone();
    bad.revision = "main".into();
    assert!(matches!(
        st.install(&bad, |_| {}).await.unwrap_err(),
        Error::Invalid(_)
    ));
    assert!(fx.srv.requests().is_empty());
}
