//! Opening files: no final symlink, no blocking on FIFOs, regular files only, and the scanned
//! size/mtime check.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use nugget_core::Result;
use nugget_extract::{
    ExtractOptions, Extracted, FileExpect, Source, extract, is_file_changed, open_regular,
};
use serde_json::json;

use crate::common::temp_dir;

/// Run `extract` on a thread and fail the test (instead of hanging) if it blocks.
fn extract_bounded(
    source: impl FnOnce() -> Result<Extracted> + Send + 'static,
) -> Result<Extracted> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(source());
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("extract blocked on the file")
}

fn opts() -> ExtractOptions {
    ExtractOptions::default()
}

fn extract_path(p: &Path) -> Result<Extracted> {
    let p = p.to_path_buf();
    extract_bounded(move || extract("text/plain", Source::File(&p), &opts()))
}

#[test]
fn regular_files_extract() {
    let dir = temp_dir("open-regular");
    let f = dir.join("a.txt");
    std::fs::write(&f, "hello file").unwrap();
    assert_eq!(extract_path(&f).unwrap().text, "hello file");
}

#[cfg(unix)]
#[test]
fn a_file_swapped_for_a_symlink_is_refused() {
    let dir = temp_dir("open-symlink");
    let outside = dir.join("outside-secret.txt");
    std::fs::write(&outside, "top secret").unwrap();
    let scanned = dir.join("root-note.txt");
    std::os::unix::fs::symlink(&outside, &scanned).unwrap();
    let err = extract_path(&scanned).unwrap_err();
    assert!(is_file_changed(&err), "{err}");
    assert!(err.to_string().contains("symbolic link"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_file_swapped_for_a_fifo_is_refused_without_blocking() {
    let dir = temp_dir("open-fifo");
    let fifo = dir.join("pipe.txt");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("mkfifo unavailable; skipping");
        return;
    }
    // No writer: a blocking open or read would hang forever.
    let err = extract_path(&fifo).unwrap_err();
    assert!(is_file_changed(&err), "{err}");
    assert!(err.to_string().contains("not a regular file"), "{err}");
}

#[test]
fn directories_are_refused() {
    let dir = temp_dir("open-dir");
    let err = match open_regular(&dir, None) {
        Err(e) => e,
        Ok(_) => panic!("opened a directory"),
    };
    // Windows refuses to open a directory without backup semantics: an I/O error is fine there.
    if cfg!(unix) {
        assert!(is_file_changed(&err), "{err}");
    }
}

#[test]
fn the_scanned_size_and_mtime_must_still_match() {
    let dir = temp_dir("open-expect");
    let f = dir.join("n.txt");
    std::fs::write(&f, "v1").unwrap();
    let expect = FileExpect::of(&std::fs::symlink_metadata(&f).unwrap());

    // Round trip through document metadata.
    let meta = json!({"path": "n.txt", nugget_extract::FILE_CHECK_KEY: expect.to_metadata()});
    assert_eq!(FileExpect::from_metadata(&meta), Some(expect));
    assert_eq!(FileExpect::from_metadata(&json!({})), None);

    let ok = extract(
        "text/plain",
        Source::CheckedFile { path: &f, expect },
        &opts(),
    )
    .unwrap();
    assert_eq!(ok.text, "v1");

    std::fs::write(&f, "version two").unwrap();
    let err = extract(
        "text/plain",
        Source::CheckedFile { path: &f, expect },
        &opts(),
    )
    .unwrap_err();
    assert!(is_file_changed(&err), "{err}");

    // Same size, different mtime.
    std::fs::write(&f, "v3").unwrap();
    let older = FileExpect {
        len: 2,
        modified_ns: expect.modified_ns.map(|t| t - 1_000_000_000),
    };
    let err = extract(
        "text/plain",
        Source::CheckedFile {
            path: &f,
            expect: older,
        },
        &opts(),
    )
    .unwrap_err();
    assert!(is_file_changed(&err), "{err}");

    // Other errors are not "changed".
    assert!(!is_file_changed(&nugget_core::Error::Invalid("x".into())));
}
