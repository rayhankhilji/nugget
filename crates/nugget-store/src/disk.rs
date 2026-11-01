//! Free space on the disk that holds the store, and whether a write recently failed because it
//! was full (`status.disk`, QA GG6-25).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nugget_core::ipc::DiskStatus;

/// A disk-full write error counts as recent for this long.
pub const RECENT_FULL_ERROR: Duration = Duration::from_secs(10 * 60);
/// Free space is read at most this often (`status` is polled).
const CACHE_FOR: Duration = Duration::from_secs(5);

/// Milliseconds since the Unix epoch of the last disk-full write error; 0 = none.
static LAST_FULL_MS: AtomicU64 = AtomicU64::new(0);
/// The last free-space reading: the directory, when it was taken, and the bytes.
static CACHE: Mutex<Option<(PathBuf, Instant, Option<u64>)>> = Mutex::new(None);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Record that a write failed because the disk is full (SQLite's `SQLITE_FULL`, `ENOSPC`).
pub(crate) fn note_disk_full() {
    LAST_FULL_MS.store(now_ms().max(1), Ordering::Relaxed);
    if let Ok(mut c) = CACHE.lock() {
        *c = None;
    }
}

/// Whether a disk-full write error happened within [`RECENT_FULL_ERROR`].
pub(crate) fn recent_full_error() -> bool {
    let at = LAST_FULL_MS.load(Ordering::Relaxed);
    at != 0 && now_ms().saturating_sub(at) < RECENT_FULL_ERROR.as_millis() as u64
}

/// Whether `e` says the disk is full.
pub(crate) fn is_disk_full(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::DiskFull
    )
}

/// Bytes available in `dir`, read at most every [`CACHE_FOR`].
fn free_bytes(dir: &Path) -> Option<u64> {
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((d, at, free)) = cache.as_ref()
        && d == dir
        && at.elapsed() < CACHE_FOR
    {
        return *free;
    }
    let free = match fs4::available_space(dir) {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::debug!(dir = %dir.display(), error = %e, "cannot read free disk space");
            None
        }
    };
    *cache = Some((dir.to_path_buf(), Instant::now(), free));
    free
}

/// Free space on the disk holding `dir`, classified (see [`DiskStatus::classify`]).
pub(crate) fn status(dir: &Path) -> DiskStatus {
    DiskStatus::classify(free_bytes(dir), recent_full_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_free_space_and_remembers_disk_full_errors() {
        let dir = tempfile::tempdir().unwrap();
        let s = status(dir.path());
        assert!(s.free_bytes.is_some_and(|b| b > 0), "{s:?}");
        let full = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
            None,
        );
        assert!(is_disk_full(&full));
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        assert!(!is_disk_full(&busy));
        note_disk_full();
        assert!(recent_full_error());
        assert!(status(dir.path()).recent_full_error);

        let key = crate::backup::test_support::key();
        let store = crate::Store::open(dir.path().join("n.db"), &key).unwrap();
        assert!(store.disk_status().unwrap().free_bytes.is_some());
        assert_eq!(crate::Store::open_in_memory().unwrap().disk_status(), None);
    }
}
