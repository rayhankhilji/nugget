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
    let root = root_in(&tmp, "empty");
    let r = scan(json!({ "roots": [root] })).await;
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
async fn scan_report_unsupported_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "pics");
    write(&root, "a.mp3", "x");
    write(&root, "b.zip", "x");
    write(&root, "sub/c.exe", "x");
    // Hidden and ignored folders are not "skipped".
    write(&root, ".hidden/d.zip", "x");
    write(&root, "node_modules/e.zip", "x");
    let r = scan(json!({ "roots": [root] })).await;
    assert_eq!(r.found, 0);
    assert_eq!(r.unsupported, 3);
    assert_eq!((r.too_large, r.unreadable, r.roots_denied), (0, 0, 0));
    assert_eq!(
        r.skipped,
        [
            skip("a.mp3", "unsupported", None),
            skip("b.zip", "unsupported", None),
            skip("sub/c.exe", "unsupported", None),
        ]
    );
}

#[tokio::test]
async fn scan_report_too_large() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "big");
    write(&root, "huge.txt", &"x".repeat(2000));
    let r = scan(json!({ "roots": [root], "max_file_bytes": 1000 })).await;
    assert_eq!(r.found, 0);
    assert_eq!(r.too_large, 1);
    assert_eq!(r.max_file_bytes, 1000);
    assert_eq!(r.skipped, [skip("huge.txt", "too_large", Some(2000))]);
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
    let r = scan(json!({ "roots": [root], "max_file_bytes": 100 })).await;
    assert_eq!(r.found, 2);
    assert_eq!(r.unsupported, 7);
    assert_eq!(r.too_large, 1);
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
async fn scan_report_unreadable_root() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "locked");
    write(&root, "a.md", "secret");
    let mut h = Harness::files(json!({ "roots": [root] }));
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(&root).is_ok();
    if !readable {
        h.sync().await;
    }
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        eprintln!("running with permission bypass (root); skipping");
        return;
    }
    let r = h.fake.scans.lock().unwrap()[0].clone();
    assert!(r.finished);
    assert_eq!((r.roots_denied, r.roots_missing, r.found), (1, 0, 0));
}

#[cfg(unix)]
#[tokio::test]
async fn scan_report_unreadable_subfolder() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = root_in(&tmp, "partly");
    write(&root, "a.md", "ok");
    write(&root, "locked/b.md", "secret");
    let mut h = Harness::files(json!({ "roots": [root] }));
    let locked = root.join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(&locked).is_ok();
    if !readable {
        h.sync().await;
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        eprintln!("running with permission bypass (root); skipping");
        return;
    }
    let r = h.fake.scans.lock().unwrap()[0].clone();
    assert_eq!((r.found, r.unreadable, r.roots_denied), (1, 1, 0));
    assert_eq!(r.skipped, [skip("locked", "unreadable", None)]);
}
