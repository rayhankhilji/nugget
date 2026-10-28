//! Live watching. Timing-tolerant: every expectation polls with a generous deadline.

use std::time::Duration;

use nugget_core::Error;
use serde_json::{Value, json};

use crate::common::*;

fn live(root: &std::path::Path) -> Value {
    json!({ "roots": [root], "debounce_ms": 100, "rescan_interval_secs": 0 })
}

#[tokio::test]
async fn create_modify_delete_are_delivered_live() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "existing.md", "e");
    let mut h = Harness::files(live(&root));
    h.sync().await;
    let (cancel, task) = h.start_watch();
    h.wait_watching(&root, "txt").await;

    let p = write(&root, "sub/new.md", "hello");
    let v1 = version(&p);
    h.wait_for("create", |c| {
        docs(c)
            .get("0:sub/new.md")
            .is_some_and(|d| d.version.as_deref() == Some(v1.as_str()))
    })
    .await;

    append(&root, "existing.md", " edited");
    let v2 = version(&path(&root, "existing.md"));
    h.wait_for("modify", |c| {
        docs(c)
            .get("0:existing.md")
            .is_some_and(|d| d.version.as_deref() == Some(v2.as_str()))
    })
    .await;

    std::fs::remove_file(&p).unwrap();
    h.wait_for("delete", |c| deletes(c).contains("0:sub/new.md"))
        .await;

    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("watch stops promptly")
        .unwrap();
    assert!(matches!(res, Err(Error::Cancelled)), "{res:?}");
}

#[tokio::test]
async fn renames_and_directory_moves_become_delete_plus_upsert() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "a.md", "a");
    write(&root, "d1/x.md", "x");
    write(&root, "d1/inner/y.md", "y");
    std::fs::create_dir_all(path(&root, "d2")).unwrap();
    let mut h = Harness::files(live(&root));
    h.sync().await;
    let (cancel, task) = h.start_watch();
    h.wait_watching(&root, "txt").await;

    std::fs::rename(path(&root, "a.md"), path(&root, "b.md")).unwrap();
    h.wait_for("file rename", |c| {
        deletes(c).contains("0:a.md") && upserts(c).contains("0:b.md")
    })
    .await;

    std::fs::rename(path(&root, "d1"), path(&root, "d2/moved")).unwrap();
    h.wait_for("directory move", |c| {
        let d = deletes(c);
        let u = upserts(c);
        d.contains("0:d1/x.md")
            && d.contains("0:d1/inner/y.md")
            && u.contains("0:d2/moved/x.md")
            && u.contains("0:d2/moved/inner/y.md")
    })
    .await;

    // Moving out of the root is a delete.
    std::fs::rename(path(&root, "b.md"), tmp.path().join("b.md")).unwrap();
    h.wait_for("move out", |c| deletes(c).contains("0:b.md"))
        .await;

    // The store converged on exactly what is on disk.
    let want = set([
        "0:d2/moved/inner/y.md",
        "0:d2/moved/x.md",
        "0:zz-sentinel.txt",
    ]);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while h.store_keys() != want {
        assert!(
            std::time::Instant::now() < deadline,
            "store {:?}",
            h.store_keys()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cancel.cancel();
    let _ = task.await.unwrap();
}

#[tokio::test]
async fn bursts_of_writes_are_coalesced() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    let mut h = Harness::files(live(&root));
    h.sync().await;
    let (cancel, task) = h.start_watch();
    h.wait_watching(&root, "txt").await;

    // An editor save storm: truncate, write, append, many times in a row.
    let p = path(&root, "burst.md");
    for i in 0..40 {
        std::fs::write(&p, format!("draft {i}\n")).unwrap();
        append(&root, "burst.md", "tail\n");
    }
    let last = version(&p);
    let c = h
        .wait_for("final version", |c| {
            c.iter().rev().find_map(|ch| match ch {
                nugget_connector::Change::Upsert(d) if d.key.0 == "0:burst.md" => {
                    Some(d.version.as_deref() == Some(last.as_str()))
                }
                _ => None,
            }) == Some(true)
        })
        .await;
    // Give stragglers a moment, then count.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let n = h
        .snapshot()
        .iter()
        .filter(|ch| matches!(ch, nugget_connector::Change::Upsert(d) if d.key.0 == "0:burst.md"))
        .count();
    eprintln!("burst: 80 writes -> {n} upserts");
    assert!(
        (1..=10).contains(&n),
        "80 writes produced {n} upserts: {:?}",
        summary(&c)
    );
    cancel.cancel();
    let _ = task.await.unwrap();
}

#[tokio::test]
async fn filtered_paths_are_not_reported_live() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, ".gitignore", "ignored/\n");
    write(&root, "sub/keep.md", "k");
    write(&root, "sub/also.md", "a");
    let mut h = Harness::files(live(&root));
    h.sync().await;
    let (cancel, task) = h.start_watch();
    h.wait_watching(&root, "txt").await;

    write(&root, "node_modules/pkg/readme.md", "x");
    write(&root, ".git/objects/ab", "x");
    write(&root, "ignored/note.md", "x");
    write(&root, ".hidden.md", "x");
    write(&root, "big-binary.exe", "x");
    write(&root, "note.md.swp", "x");
    tokio::time::sleep(Duration::from_millis(300)).await;
    write(&root, "control.md", "c");
    let c = h
        .wait_for("control file", |c| upserts(c).contains("0:control.md"))
        .await;
    assert_eq!(upserts(&c), set(["0:control.md"]), "{:?}", summary(&c));

    // Editing an ignore file re-evaluates its directory.
    write(&root, ".gitignore", "ignored/\nsub/\n");
    h.wait_for("ignore-file change", |c| {
        let d = deletes(c);
        d.contains("0:sub/keep.md") && d.contains("0:sub/also.md")
    })
    .await;
    cancel.cancel();
    let _ = task.await.unwrap();
}

#[tokio::test]
async fn watch_start_catches_changes_since_the_last_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "a.md", "a");
    write(&root, "b.md", "b");
    let mut h = Harness::files(live(&root));
    h.sync().await;

    // Changes between the end of the sync pass and the watcher starting.
    std::fs::remove_file(path(&root, "a.md")).unwrap();
    write(&root, "c.md", "c");
    let (cancel, task) = h.start_watch();
    h.wait_for("catch-up", |c| {
        deletes(c).contains("0:a.md") && upserts(c).contains("0:c.md")
    })
    .await;
    assert!(
        !upserts(&h.snapshot()).contains("0:b.md"),
        "unchanged files are not re-sent"
    );
    cancel.cancel();
    let _ = task.await.unwrap();
}

#[tokio::test]
async fn watch_without_a_prior_sync_uses_the_store() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "r");
    write(&root, "a.md", "a");
    let mut h = Harness::files(live(&root));
    h.sync().await;
    // A new connector instance (no in-memory state) watching straight away.
    h.restart(live(&root));
    let (cancel, task) = h.start_watch();
    h.wait_watching(&root, "txt").await;
    assert!(
        !upserts(&h.snapshot()).contains("0:a.md"),
        "known versions came from KnownItems"
    );
    std::fs::remove_file(path(&root, "a.md")).unwrap();
    h.wait_for("delete", |c| deletes(c).contains("0:a.md"))
        .await;
    cancel.cancel();
    let _ = task.await.unwrap();
}
