//! Supervisor behaviour: backoff, NeedsAuth, auth flows, power, watch, events and shutdown.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::ipc::{IngestPhase, SourceState};
use nugget_ingest::{IngestConfig, IngestEvent};
use serde_json::json;

fn id() -> SourceId {
    SourceId::new("fake-1")
}

#[tokio::test]
async fn failed_passes_retry_with_growing_backoff_then_recover() {
    let h = start(Opts::default()).await; // backoff 20 ms base, 200 ms cap
    let st = h.factory.state("fake-1");
    st.script(vec![Step::Fail(ErrKind::Network)]);
    st.script(vec![Step::Fail(ErrKind::Network)]);
    st.script(vec![Step::Fail(ErrKind::Network)]);
    st.script(vec![
        Step::Send(text_doc("a", "finally", Some("1"))),
        Step::Return(json!("ok")),
    ]);
    h.add("fake-1").await;
    eventually("recovery", || h.cursor("fake-1") == Some(json!("ok"))).await;
    h.wait_state("fake-1", SourceState::Idle).await;

    assert_eq!(st.calls(), 4);
    let t = st.sync_times.lock().unwrap().clone();
    let gaps: Vec<Duration> = t.windows(2).map(|w| w[1] - w[0]).collect();
    // Nominal 20, 40, 80 ms with ±25 % jitter.
    assert!(gaps[0] >= Duration::from_millis(15), "{gaps:?}");
    assert!(gaps[1] >= Duration::from_millis(30), "{gaps:?}");
    assert!(gaps[2] >= Duration::from_millis(60), "{gaps:?}");
    let info = h.ingest.get(&id()).await.unwrap();
    assert_eq!(info.last_error, None);
    assert!(info.last_sync.is_some());
    let errors = h.ingest.errors(&id()).unwrap();
    assert_eq!(errors.len(), 3);
    assert!(
        errors
            .iter()
            .all(|e| e.key.is_none() && e.message.contains("network"))
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn backoff_is_capped() {
    let h = start(Opts {
        cfg: IngestConfig {
            backoff_base_ms: 10,
            backoff_max_ms: 40,
            ..cfg()
        },
        ..Default::default()
    })
    .await;
    let st = h.factory.state("fake-1");
    for _ in 0..8 {
        st.script(vec![Step::Fail(ErrKind::Network)]);
    }
    h.add("fake-1").await;
    eventually("9 attempts", || st.calls() >= 9).await;
    let t = st.sync_times.lock().unwrap().clone();
    for w in t.windows(2).skip(3) {
        assert!(
            w[1] - w[0] < Duration::from_millis(400),
            "{:?}",
            w[1] - w[0]
        );
    }
    h.wait_state("fake-1", SourceState::Idle).await;
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn unauthorized_parks_the_source_until_credentials_change() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.script(vec![Step::Fail(ErrKind::Unauthorized)]);
    st.script(vec![
        Step::Send(text_doc("m", "mail body", Some("1"))),
        Step::Return(json!("ok")),
    ]);
    h.add("fake-1").await;
    h.wait_state("fake-1", SourceState::NeedsAuth).await;
    let info = h.ingest.get(&id()).await.unwrap();
    assert!(info.last_error.unwrap().contains("authorization"));

    // No retries while waiting for the user.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(st.calls(), 1);

    h.ingest.set_password(&id(), "app-password").await.unwrap();
    eventually("resync", || h.cursor("fake-1") == Some(json!("ok"))).await;
    h.wait_state("fake-1", SourceState::Idle).await;
    assert_eq!(st.calls(), 2);

    // The value lives only in the secret store; sources.toml records the account name.
    assert_eq!(
        nugget_connector::SecretStore::get(&h.secrets, "password:fake-1").unwrap(),
        Some("app-password".into())
    );
    let file = std::fs::read_to_string(h.file_path()).unwrap();
    assert!(file.contains("password:fake-1"));
    assert!(!file.contains("app-password"));

    // Accounts of other sources (or providers) are refused.
    assert!(
        h.ingest
            .set_secret(&id(), "password:fake-2", "x")
            .await
            .is_err()
    );
    assert!(
        h.ingest
            .set_secret(&id(), "provider:anthropic", "x")
            .await
            .is_err()
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn begin_auth_completes_and_resyncs() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.script(vec![Step::Fail(ErrKind::Unauthorized)]);
    st.script(vec![Step::Return(json!("authed"))]);
    let (tx, rx) = tokio::sync::oneshot::channel();
    *st.auth_done.lock().unwrap() = Some(rx);
    h.add("fake-1").await;
    h.wait_state("fake-1", SourceState::NeedsAuth).await;

    let flow = h.ingest.begin_auth(&id()).await.unwrap();
    assert!(flow.url.starts_with("https://auth.example/"));
    tx.send(()).unwrap();
    flow.done.await.unwrap();
    eventually("resync after auth", || {
        h.cursor("fake-1") == Some(json!("authed"))
    })
    .await;
    h.wait_state("fake-1", SourceState::Idle).await;
    // The connector stored its token through the scoped secret store, and it was recorded.
    assert_eq!(h.secrets.accounts(), vec!["oauth:fake-1".to_owned()]);
    assert!(
        std::fs::read_to_string(h.file_path())
            .unwrap()
            .contains("oauth:fake-1")
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn backfills_pause_on_low_power_until_overridden() {
    let power = Arc::new(TogglePower::default());
    power.0.store(true, Ordering::SeqCst);
    let h = start(Opts {
        power: power.clone(),
        ..Default::default()
    })
    .await;
    let st = h.factory.state("fake-1");
    st.script(vec![
        Step::Send(text_doc("a", "alpha", Some("1"))),
        Step::Return(json!(1)),
    ]);
    h.add("fake-1").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(st.calls(), 0, "no backfill on low power");
    assert!(h.ingest.is_paused(&id()));

    // Power comes back: the backfill runs.
    power.0.store(false, Ordering::SeqCst);
    eventually("backfill", || h.cursor("fake-1") == Some(json!(1))).await;
    assert!(!h.ingest.is_paused(&id()));

    // Incremental passes (with a cursor) run regardless of power.
    power.0.store(true, Ordering::SeqCst);
    st.script(vec![
        Step::Send(text_doc("b", "bravo", Some("1"))),
        Step::Return(json!(2)),
    ]);
    h.ingest.sync_now(Some(&id())).unwrap();
    eventually("incremental", || h.cursor("fake-1") == Some(json!(2))).await;
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn sync_now_overrides_the_power_policy() {
    let power = Arc::new(TogglePower::default());
    power.0.store(true, Ordering::SeqCst);
    let h = start(Opts {
        power,
        ..Default::default()
    })
    .await;
    let st = h.factory.state("fake-1");
    st.script(vec![
        Step::Send(text_doc("a", "alpha", Some("1"))),
        Step::Return(json!(1)),
    ]);
    h.add("fake-1").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(st.calls(), 0);
    h.ingest.sync_now(Some(&id())).unwrap();
    eventually("forced backfill", || h.cursor("fake-1") == Some(json!(1))).await;
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn watch_runs_after_sync_and_sync_now_reconciles() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.watch.store(true, Ordering::SeqCst);
    st.script(vec![Step::Return(json!(1))]);
    st.script(vec![Step::RecordKnown, Step::Return(json!(2))]);
    h.add("fake-1").await;
    h.wait_state("fake-1", SourceState::Watching).await;
    assert_eq!(st.watch_calls.load(Ordering::SeqCst), 1);

    st.live(text_doc("w", "watched", Some("v9")));
    eventually("live item", || h.item("fake-1", "w").is_some()).await;
    // Live changes do not move the cursor.
    assert_eq!(h.cursor("fake-1"), Some(json!(1)));

    h.ingest.sync_now(None).unwrap();
    eventually("second pass", || h.cursor("fake-1") == Some(json!(2))).await;
    assert!(st.watch_cancelled.load(Ordering::SeqCst) >= 1);
    // The reconciling pass saw stored versions through KnownItems, from the cursor.
    let known = st.known.lock().unwrap().clone().unwrap();
    assert_eq!(known.get(&ItemKey::new("w")), Some(&Some("v9".to_owned())));
    assert_eq!(*st.cursors_seen.lock().unwrap(), vec![None, Some(json!(1))]);
    h.wait_state("fake-1", SourceState::Watching).await;
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn polling_sources_resync_after_the_poll_interval() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    *st.poll_interval.lock().unwrap() = Duration::from_millis(50);
    h.add("fake-1").await;
    eventually("three polls", || st.calls() >= 3).await;
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn events_report_progress_and_state_changes() {
    let h = start(Opts::default()).await;
    let mut rx = h.ingest.subscribe();
    h.factory.state("fake-1").script(vec![
        Step::Send(text_doc("a", "alpha", Some("1"))),
        Step::Return(json!(1)),
    ]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!(1))).await;
    h.wait_state("fake-1", SourceState::Idle).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut phases = Vec::new();
    let mut states = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        match ev {
            IngestEvent::Progress(p) => {
                assert_eq!(p.source, id());
                phases.push(p.phase);
            }
            IngestEvent::SourceChanged(s) => states.push(s.state),
            IngestEvent::SourceRemoved(_) => panic!("unexpected removal"),
        }
    }
    assert!(phases.contains(&IngestPhase::Scanning), "{phases:?}");
    assert!(phases.contains(&IngestPhase::Indexing), "{phases:?}");
    assert!(phases.contains(&IngestPhase::Idle), "{phases:?}");
    assert!(states.contains(&SourceState::Syncing), "{states:?}");
    assert_eq!(states.last(), Some(&SourceState::Idle), "{states:?}");
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn shutdown_stops_watchers_and_hung_connectors() {
    let h = start(Opts::default()).await;
    let watcher = h.factory.state("fake-1");
    watcher.watch.store(true, Ordering::SeqCst);
    let hung = h.factory.state("fake-2");
    hung.script(vec![Step::Hang]);
    h.add("fake-1").await;
    h.add("fake-2").await;
    h.wait_state("fake-1", SourceState::Watching).await;
    eventually("hung pass started", || hung.calls() == 1).await;

    tokio::time::timeout(Duration::from_secs(5), h.ingest.shutdown())
        .await
        .expect("shutdown finishes");
    assert_eq!(watcher.watch_cancelled.load(Ordering::SeqCst), 1);
    // Idempotent, and the handle stays usable for reads.
    h.ingest.shutdown().await;
    assert_eq!(h.ingest.list().await.unwrap().len(), 2);
    h.ingest.sync_now(None).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(hung.calls(), 1, "nothing restarts after shutdown");
}
