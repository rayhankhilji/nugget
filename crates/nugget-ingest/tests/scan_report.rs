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
        Step::Scan(finished(2)),
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
