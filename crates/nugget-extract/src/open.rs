//! Opening a local file for extraction without following a swapped-in symlink or blocking on a
//! FIFO or device.
//!
//! Connectors scan with `lstat` and the pipeline opens the path later, so the path may have been
//! replaced in between (a symlink to a file outside the source, a FIFO that never yields data).
//! [`open_regular`] opens the last path component without following it (`O_NOFOLLOW`, or
//! `FILE_FLAG_OPEN_REPARSE_POINT` on Windows) and without blocking (`O_NONBLOCK`), then checks the
//! *opened handle* (`fstat`) is a regular file and, optionally, still has the scanned size and
//! modification time ([`FileExpect`]).

use std::fs::{File, Metadata, OpenOptions};
use std::path::Path;
use std::time::UNIX_EPOCH;

use nugget_core::{Error, Result};
use serde_json::{Value, json};

/// Prefix of the `Error::Integrity` message for a file that is no longer what was scanned (see
/// [`is_file_changed`]).
pub const FILE_CHANGED: &str = "file changed since it was scanned";

/// Metadata key under which a connector records the [`FileExpect`] of a `Content::File`
/// document (`{"len": <bytes>, "mtime_ns": <ns since the Unix epoch>}`).
pub const FILE_CHECK_KEY: &str = "file_check";

/// What a file looked like when a connector scanned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileExpect {
    pub len: u64,
    /// Modification time in nanoseconds since the Unix epoch (negative before 1970). `None`
    /// skips the time check.
    pub modified_ns: Option<i64>,
}

impl FileExpect {
    /// The size and modification time in `md`.
    pub fn of(md: &Metadata) -> Self {
        Self {
            len: md.len(),
            modified_ns: modified_ns(md),
        }
    }

    /// The JSON stored under [`FILE_CHECK_KEY`].
    pub fn to_metadata(&self) -> Value {
        match self.modified_ns {
            Some(ns) => json!({"len": self.len, "mtime_ns": ns}),
            None => json!({"len": self.len}),
        }
    }

    /// Read the expectation a connector left in a document's metadata object, if any.
    pub fn from_metadata(metadata: &Value) -> Option<Self> {
        let v = metadata.get(FILE_CHECK_KEY)?;
        Some(Self {
            len: v.get("len")?.as_u64()?,
            modified_ns: v.get("mtime_ns").and_then(Value::as_i64),
        })
    }
}

fn modified_ns(md: &Metadata) -> Option<i64> {
    let t = md.modified().ok()?;
    let ns: i128 = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    i64::try_from(ns).ok()
}

/// Whether `e` is the error [`open_regular`] (and so [`extract`](crate::extract) on a
/// [`Source::CheckedFile`](crate::Source::CheckedFile)) returns for a file that changed since it
/// was scanned: swapped for a symlink, a FIFO, a device or a directory, or (with a
/// [`FileExpect`]) a different size or modification time. The pipeline should skip such an item
/// and let the connector's next pass report the file as it is now.
pub fn is_file_changed(e: &Error) -> bool {
    matches!(e, Error::Integrity(m) if m.starts_with(FILE_CHANGED))
}

fn changed(path: &Path, what: &str) -> Error {
    Error::Integrity(format!("{FILE_CHANGED}: `{}` {what}", path.display()))
}

/// Open `path` for reading, refusing anything but a regular file, without following a final
/// symlink and without blocking. With `expect`, the opened file must also have the scanned size
/// and modification time. Refusals are `Error::Integrity` (see [`is_file_changed`]).
pub fn open_regular(path: &Path, expect: Option<FileExpect>) -> Result<File> {
    let file = match options().open(path) {
        Ok(f) => f,
        Err(e) => {
            // ELOOP (Linux, macOS) / EMLINK (FreeBSD) on a final symlink; check directly.
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(changed(path, "is now a symbolic link"));
            }
            return Err(Error::Io(e));
        }
    };
    let md = file.metadata()?;
    if md.file_type().is_symlink() {
        return Err(changed(path, "is now a symbolic link"));
    }
    if !md.is_file() {
        return Err(changed(
            path,
            "is not a regular file (a directory, FIFO, socket or device)",
        ));
    }
    if let Some(want) = expect {
        let now = FileExpect::of(&md);
        if now.len != want.len
            || (want.modified_ns.is_some()
                && now.modified_ns.is_some()
                && now.modified_ns != want.modified_ns)
        {
            return Err(changed(path, "has a different size or modification time"));
        }
    }
    Ok(file)
}

#[cfg(unix)]
fn options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = OpenOptions::new();
    // O_NONBLOCK: opening a FIFO without a writer, or a device, must not hang (it has no effect
    // on reads of regular files). O_NOCTTY: never acquire a terminal.
    o.read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY);
    o
}

#[cfg(windows)]
fn options() -> OpenOptions {
    use std::os::windows::fs::OpenOptionsExt;
    /// `FILE_FLAG_OPEN_REPARSE_POINT` (winbase.h): open a symlink or junction itself.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut o = OpenOptions::new();
    o.read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    o
}

#[cfg(not(any(unix, windows)))]
fn options() -> OpenOptions {
    let mut o = OpenOptions::new();
    o.read(true);
    o
}
