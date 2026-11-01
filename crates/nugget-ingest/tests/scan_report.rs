//! GG6-2 / GG6-26: scan reports reach `SourceInfo.scan`, drive the `nothing_indexable` and
//! `unreadable` health codes, and give indexing progress a total.

mod common;

use common::*;
use nugget_core::ids::SourceId;
use nugget_core::ipc::{IngestPhase, ScanReport, SkippedFile, SourceState};
use nugget_ingest::IngestEvent;
use serde_json::json;

fn id() -> SourceId {
    SourceId::new("fake-1")
}

fn finished(found: u64) -> ScanReport {
    ScanReport {
        finished: true,
        found,
        max_file_bytes: 50 * 1024 * 1024,
        ..Default::default()
    }
}

async fn settle(h: &Harness, cursor: serde_json::Value) {
    eventually("pass", || h.cursor("fake-1") == Some(cursor.clone())).await;
    h.wait_state("fake-1", SourceState::Idle).await;
}

#[tokio::test]
async fn nothing_indexable_after_a_finished_empty_scan() {
    let h = start(Opts::default()).await;
    let report = ScanReport {
        unsupported: 2,
        too_large: 1,
        skipped: vec![SkippedFile {
            path: "movie.mov".into(),
            reason: "unsupported".into(),
            bytes: None,
        }],
        ..finished(0)
    };
    h.factory
        .state("fake-1")
        .script(vec![Step::Scan(report.clone()), Step::Return(json!(1))]);
    h.add("fake-1").await;
    settle(&h, json!(1)).await;

    let info = h.ingest.get(&id()).await.unwrap();
    assert_eq!(info.items, 0);
    assert_eq!(info.scan.as_ref(), Some(&report));
    let health = info.health.unwrap();
    assert_eq!(health.code, "nothing_indexable");
    assert_eq!(health.reason, "Nothing in this folder can be indexed yet.");
    assert!(
        health.fix.contains("2 files are of a type"),
        "{}",
        health.fix
    );
    assert!(health.fix.contains("50 MB"), "{}", health.fix);

    // On the wire: `scan` is an object with every field.
    let v = serde_json::to_value(h.ingest.get(&id()).await.unwrap()).unwrap();
    assert_eq!(v["scan"]["found"], 0);
    assert_eq!(v["scan"]["finished"], true);
    assert_eq!(v["scan"]["skipped"][0]["path"], "movie.mov");
    assert!(v["scan"]["skipped"][0].get("bytes").is_none());
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn empty_folder_says_so() {
    let h = start(Opts::default()).await;
    h.factory
        .state("fake-1")
        .script(vec![Step::Scan(finished(0)), Step::Return(json!(1))]);
    h.add("fake-1").await;
    settle(&h, json!(1)).await;
    let health = h.ingest.get(&id()).await.unwrap().health.unwrap();
    assert_eq!(health.code, "nothing_indexable");
    assert!(
        health.fix.starts_with("The folder is empty."),
        "{}",
        health.fix
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn unfinished_scan_or_found_files_stay_ok() {
    let h = start(Opts::default()).await;
    h.factory.state("fake-1").script(vec![
        Step::Scan(ScanReport::default()),
        Step::Return(json!(1)),
    ]);
    h.add("fake-1").await;
    settle(&h, json!(1)).await;
    let info = h.ingest.get(&id()).await.unwrap();
    assert_eq!(info.health.unwrap().code, "ok");
    assert_eq!(info.scan.map(|s| s.finished), Some(false));

    let h2 = start(Opts::default()).await;
    h2.factory.state("fake-1").script(vec![
        Step::Scan(finished(1)),
        Step::Send(text_doc("a", "alpha", Some("1"))),
        Step::Return(json!(1)),
    ]);
    h2.add("fake-1").await;
    settle(&h2, json!(1)).await;
    assert_eq!(
        h2.ingest.get(&id()).await.unwrap().health.unwrap().code,
        "ok"
    );
    h.ingest.shutdown().await;
    h2.ingest.shutdown().await;
}

#[tokio::test]
async fn denied_root_is_unreadable() {
    let h = start(Opts::default()).await;
    h.factory.state("fake-1").script(vec![
        Step::Scan(ScanReport {
            roots_denied: 1,
            ..finished(0)
        }),
        Step::Return(json!(1)),
    ]);
    h.add("fake-1").await;
    settle(&h, json!(1)).await;
    let health = h.ingest.get(&id()).await.unwrap().health.unwrap();
    assert_eq!(health.code, "unreadable");
    assert!(
        health.reason.contains("open this folder"),
        "{}",
        health.reason
    );
    assert!(health.fix.contains("permissions"), "{}", health.fix);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn progress_carries_the_scan_total() {
    let h = start(Opts::default()).await;
    let mut rx = h.ingest.subscribe();
    h.factory.state("fake-1").script(vec![
        Step::Scan(ScanReport {
            to_index: 2,
            ..finished(2)
        }),
        Step::Send(text_doc("a", "alpha", Some("1"))),
        Step::Send(text_doc("b", "beta", Some("1"))),
        Step::Return(json!(1)),
    ]);
    h.add("fake-1").await;
    settle(&h, json!(1)).await;
    let mut totals = Vec::new();
    let mut scans = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        match ev {
            IngestEvent::Progress(p) if p.phase == IngestPhase::Indexing => totals.push(p.total),
            IngestEvent::SourceChanged(s) => scans.push(s.scan),
            _ => {}
        }
    }
    assert!(!totals.is_empty());
    assert!(totals.iter().all(|t| *t == Some(2)), "{totals:?}");
    // The report is announced as soon as it arrives.
    assert!(
        scans
            .iter()
            .any(|s| s.as_ref().is_some_and(|s| s.found == 2))
    );
    h.ingest.shutdown().await;
}

/// The total is what the pass sends, not every file found: a second pass over an indexed
/// folder with one changed file counts to 1, not to the folder's size.
#[tokio::test]
async fn second_pass_total_counts_only_what_it_sends() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.script(vec![
        Step::Scan(ScanReport {
            to_index: 3,
            ..finished(3)
        }),
        Step::Send(text_doc("a", "alpha", Some("1"))),
        Step::Send(text_doc("b", "beta", Some("1"))),
        Step::Send(text_doc("c", "gamma", Some("1"))),
        Step::Return(json!(1)),
    ]);
    h.add("fake-1").await;
    settle(&h, json!(1)).await;

    let mut rx = h.ingest.subscribe();
    st.script(vec![
        Step::Scan(ScanReport {
            to_index: 1,
            ..finished(3)
        }),
        Step::Send(text_doc("b", "beta two", Some("2"))),
        Step::Return(json!(2)),
    ]);
    h.ingest.sync_now(Some(&id())).unwrap();
    settle(&h, json!(2)).await;
    let mut totals = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let IngestEvent::Progress(p) = ev
            && p.phase == IngestPhase::Indexing
        {
            totals.push((p.done, p.total));
        }
    }
    assert_eq!(totals.last(), Some(&(1, Some(1))), "{totals:?}");
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn changing_settings_clears_the_report() {
    let h = start(Opts::default()).await;
    h.factory
        .state("fake-1")
        .script(vec![Step::Scan(finished(0)), Step::Return(json!(1))]);
    h.add("fake-1").await;
    settle(&h, json!(1)).await;
    assert!(h.ingest.get(&id()).await.unwrap().scan.is_some());

    let info = h
        .ingest
        .update(nugget_core::ipc::SourceUpdate {
            id: id(),
            name: None,
            enabled: None,
            settings: Some(json!({"root": "/tmp/elsewhere"})),
        })
        .await
        .unwrap();
    assert_eq!(info.scan, None);
    assert_ne!(info.health.unwrap().code, "nothing_indexable");

    // Turning the source off and on clears it too.
    h.factory
        .state("fake-1")
        .script(vec![Step::Scan(finished(0)), Step::Return(json!(2))]);
    h.ingest.sync_now(Some(&id())).unwrap();
    eventually_async("report back", || async {
        h.ingest.get(&id()).await.unwrap().scan.is_some()
    })
    .await;
    let info = h.ingest.set_enabled(&id(), false).await.unwrap();
    assert_eq!(info.scan, None);
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn an_earlier_report_does_not_hide_a_running_pass() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.script(vec![Step::Scan(finished(0)), Step::Return(json!(1))]);
    h.add("fake-1").await;
    settle(&h, json!(1)).await;
    st.script(vec![Step::Hang]);
    h.ingest.sync_now(Some(&id())).unwrap();
    h.wait_state("fake-1", SourceState::Syncing).await;
    let info = h.ingest.get(&id()).await.unwrap();
    assert_eq!(info.health.unwrap().code, "syncing");
    h.ingest.shutdown().await;
}

/// One folder of several cannot be opened: the source still works, and says so.
#[tokio::test]
async fn a_denied_folder_next_to_a_healthy_one_is_not_unreadable() {
    let h = start(Opts::default()).await;
    h.factory.state("fake-1").script(vec![
        Step::Scan(ScanReport {
            roots_denied: 1,
            to_index: 1,
            ..finished(1)
        }),
        Step::Send(text_doc("a", "alpha", Some("1"))),
        Step::Return(json!(1)),
    ]);
    h.add("fake-1").await;
    settle(&h, json!(1)).await;
    let health = h.ingest.get(&id()).await.unwrap().health.unwrap();
    assert_eq!(health.code, "ok");
    assert!(
        health.reason.contains("isn't allowed to open one"),
        "{}",
        health.reason
    );
    assert!(health.fix.contains("permissions"), "{}", health.fix);
    h.ingest.shutdown().await;
}

/// A live upsert after a pass that found nothing drops the outdated "nothing found" report.
#[tokio::test]
async fn a_live_file_drops_an_empty_report() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.watch.store(true, std::sync::atomic::Ordering::SeqCst);
    st.script(vec![Step::Scan(finished(0)), Step::Return(json!(1))]);
    h.add("fake-1").await;
    eventually("pass", || h.cursor("fake-1") == Some(json!(1))).await;
    h.wait_state("fake-1", SourceState::Watching).await;
    assert!(h.ingest.get(&id()).await.unwrap().scan.is_some());
    st.live(text_doc("new", "fresh", Some("1")));
    eventually_async("report dropped", || async {
        h.ingest.get(&id()).await.unwrap().scan.is_none()
    })
    .await;
    h.ingest.shutdown().await;
}
