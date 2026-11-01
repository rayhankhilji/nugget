//! GG6-2 / GG6-26: scan reports reach `SourceInfo.scan`, drive the `nothing_indexable` and
//! `unreadable` health codes, and give indexing progress a total.

mod common;

use common::*;
use nugget_core::ids::SourceId;
use nugget_core::ipc::{IngestPhase, ScanReport, SourceState};
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

async fn settle(h: &Harness, source: &str, cursor: serde_json::Value) {
    eventually("pass", || h.cursor(source) == Some(cursor.clone())).await;
    h.wait_state(source, SourceState::Idle).await;
}

#[tokio::test]
async fn scan_reports_drive_source_health() {
    let h = start(Opts::default()).await;
    // (report, files sent, code, text in reason or fix)
    let cases: Vec<(ScanReport, usize, &str, &[&str])> = vec![
        (
            ScanReport {
                unsupported: 2,
                too_large: 1,
                ..finished(0)
            },
            0,
            "nothing_indexable",
            &[
                "Nothing in this folder can be indexed yet.",
                "2 files are of a type",
                "50 MB",
            ],
        ),
        (
            finished(0),
            0,
            "nothing_indexable",
            &["The folder is empty."],
        ),
        (ScanReport::default(), 0, "ok", &[]),
        (finished(1), 1, "ok", &[]),
        (
            ScanReport {
                roots_denied: 1,
                ..finished(0)
            },
            0,
            "unreadable",
            &["open this folder", "permissions"],
        ),
        // One folder of several cannot be opened: the source still works, and says so.
        (
            ScanReport {
                roots_denied: 1,
                to_index: 1,
                ..finished(1)
            },
            1,
            "ok",
            &["isn't allowed to open one", "permissions"],
        ),
    ];
    for (i, (report, files, code, texts)) in cases.iter().enumerate() {
        let src = format!("s{i}");
        let mut steps = vec![Step::Scan(report.clone())];
        steps.extend(
            (0..*files).map(|f| Step::Send(text_doc(&format!("f{f}"), "alpha", Some("1")))),
        );
        steps.push(Step::Return(json!(1)));
        h.factory.state(&src).script(steps);
        h.add(&src).await;
        settle(&h, &src, json!(1)).await;
        let info = h.ingest.get(&SourceId::new(&src)).await.unwrap();
        assert_eq!(info.scan.as_ref(), Some(report), "case {i}");
        let health = info.health.unwrap();
        assert_eq!(health.code, *code, "case {i}");
        let both = format!("{} {}", health.reason, health.fix);
        for t in *texts {
            assert!(both.contains(t), "case {i}: {both}");
        }
    }
    h.ingest.shutdown().await;
}

/// The total is what the pass sends, not every file found: a second pass over an indexed
/// folder with one changed file counts to 1, not to the folder's size.
#[tokio::test]
async fn progress_carries_the_total_of_what_the_pass_sends() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    for (pass, to_index) in [(1, 3), (2, 1)] {
        let mut rx = h.ingest.subscribe();
        let mut steps = vec![Step::Scan(ScanReport {
            to_index,
            ..finished(3)
        })];
        steps.extend(
            (0..to_index)
                .map(|f| Step::Send(text_doc(&format!("f{f}"), "alpha", Some(&pass.to_string())))),
        );
        steps.push(Step::Return(json!(pass)));
        st.script(steps);
        if pass == 1 {
            h.add("fake-1").await;
        } else {
            h.ingest.sync_now(Some(&id())).unwrap();
        }
        settle(&h, "fake-1", json!(pass)).await;
        let mut totals = Vec::new();
        let mut scans = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            match ev {
                IngestEvent::Progress(p) if p.phase == IngestPhase::Indexing => {
                    totals.push((p.done, p.total))
                }
                IngestEvent::SourceChanged(s) => scans.push(s.scan),
                _ => {}
            }
        }
        assert_eq!(
            totals.last(),
            Some(&(to_index, Some(to_index))),
            "{totals:?}"
        );
        assert!(totals.iter().all(|t| t.1 == Some(to_index)), "{totals:?}");
        // The report is announced as soon as it arrives.
        assert!(
            scans
                .iter()
                .any(|s| s.as_ref().is_some_and(|s| s.found == 3))
        );
    }
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn changing_settings_or_disabling_clears_the_report() {
    let h = start(Opts::default()).await;
    let st = h.factory.state("fake-1");
    st.script(vec![Step::Scan(finished(0)), Step::Return(json!(1))]);
    h.add("fake-1").await;
    settle(&h, "fake-1", json!(1)).await;
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

    st.script(vec![Step::Scan(finished(0)), Step::Return(json!(2))]);
    h.ingest.sync_now(Some(&id())).unwrap();
    settle(&h, "fake-1", json!(2)).await;
    assert!(h.ingest.get(&id()).await.unwrap().scan.is_some());
    // Turning the source off clears it.
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
    settle(&h, "fake-1", json!(1)).await;
    st.script(vec![Step::Hang]);
    h.ingest.sync_now(Some(&id())).unwrap();
    h.wait_state("fake-1", SourceState::Syncing).await;
    let info = h.ingest.get(&id()).await.unwrap();
    assert_eq!(info.health.unwrap().code, "syncing");
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
