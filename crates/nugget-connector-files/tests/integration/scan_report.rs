//! GG6-2 / GG6-26: a pass reports what it found and what it skipped, so a folder with nothing
//! indexable can be told apart from one still being scanned.

use nugget_core::ipc::{ScanReport, SkippedFile};
use serde_json::json;

use crate::common::*;

async fn scan(settings: serde_json::Value) -> ScanReport {
    let mut h = Harness::files(settings);
    h.sync().await;
    let scans = h.fake.scans.lock().unwrap().clone();
    assert_eq!(scans.len(), 1, "one report per pass: {scans:?}");
    let r = scans.into_iter().next().unwrap();
    assert!(r.finished);
    r
}

fn skip(path: &str, reason: &str, bytes: Option<u64>) -> SkippedFile {
    SkippedFile {
        path: path.into(),
        reason: reason.into(),
        bytes,
    }
}

#[tokio::test]
async fn scan_report_empty_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let r = scan(json!({ "roots": [root_in(&tmp, "empty")] })).await;
    assert_eq!(
        r,
        ScanReport {
            finished: true,
            max_file_bytes: r.max_file_bytes,
            ..Default::default()
        }
    );
    assert!(r.max_file_bytes > 0);
}

#[tokio::test]
async fn scan_report_mixed_folder_names_examples_first_found_first() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "mixed");
    write(&root, "a.md", "hello");
    write(&root, "b.mov", "x");
    write(&root, "c.txt", &"x".repeat(200));
    write(&root, "d/e.md", "hi");
    for i in 0..6 {
        write(&root, &format!("z{i}.bin"), "x");
    }
    // Hidden and ignored folders are not "skipped".
    write(&root, ".hidden/d.zip", "x");
    write(&root, "node_modules/e.zip", "x");
    let r = scan(json!({ "roots": [root], "max_file_bytes": 100 })).await;
    assert_eq!((r.found, r.unsupported, r.too_large), (2, 7, 1));
    assert_eq!(
        (r.unreadable, r.roots_denied, r.max_file_bytes),
        (0, 0, 100)
    );
    assert_eq!(
        r.skipped,
        [
            skip("b.mov", "unsupported", None),
            skip("c.txt", "too_large", Some(200)),
            skip("z0.bin", "unsupported", None),
            skip("z1.bin", "unsupported", None),
            skip("z2.bin", "unsupported", None),
        ]
    );
}

#[tokio::test]
async fn scan_report_missing_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "gone");
    let mut h = Harness::files(json!({ "roots": [root] }));
    std::fs::remove_dir(&root).unwrap();
    h.sync().await;
    let r = h.fake.scans.lock().unwrap()[0].clone();
    assert!(r.finished);
    assert_eq!((r.roots_missing, r.roots_denied, r.found), (1, 0, 0));
}

#[cfg(unix)]
#[tokio::test]
async fn scan_report_unreadable_root_and_subfolder() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let (root, other) = (root_in(&tmp, "locked"), root_in(&tmp, "partly"));
    write(&root, "a.md", "secret");
    write(&other, "a.md", "ok");
    write(&other, "locked/b.md", "secret");
    let mut h = Harness::files(json!({ "roots": [root, other] }));
    let locked = [root.clone(), other.join("locked")];
    for p in &locked {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let readable = std::fs::read_dir(&root).is_ok();
    if !readable {
        h.sync().await;
    }
    for p in &locked {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    if readable {
        eprintln!("running with permission bypass (root); skipping");
        return;
    }
    let r = h.fake.scans.lock().unwrap()[0].clone();
    assert!(r.finished);
    assert_eq!((r.found, r.unreadable, r.roots_denied), (1, 1, 1));
    assert_eq!(r.skipped, [skip("locked", "unreadable", None)]);
}

/// `to_index` counts what a pass sends (upserts and deletes), not every file found.
#[tokio::test]
async fn scan_report_to_index_counts_only_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "docs");
    for f in ["a.md", "b.md", "c.md"] {
        write(&root, f, f);
    }
    let mut h = Harness::files(json!({ "roots": [root] }));
    h.sync().await;
    write(&root, "b.md", "b changed");
    std::fs::remove_file(root.join("c.md")).unwrap();
    h.sync().await;
    h.sync().await;
    let got: Vec<_> = h
        .fake
        .scans
        .lock()
        .unwrap()
        .iter()
        .map(|s| (s.found, s.to_index))
        .collect();
    assert_eq!(got, [(3, 3), (2, 2), (2, 0)]);
}
