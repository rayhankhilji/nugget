//! Path resolution for `nugget:plugin/fs`: a plugin names a file as a root (a folder or file the
//! user chose) plus a relative path, and must not reach anything outside that root.
//!
//! Two layers, both pure apart from [`resolve`]'s `canonicalize` calls:
//! - [`check_relative`] checks the syntax of the relative path only: `/`-separated segments, no
//!   `.`/`..`, no empty segments (so no leading `/`), no `\`, `:` (drive letters, Windows
//!   alternate data streams), NUL or other control characters, and no Windows device names.
//!   The rules are the same on every OS, so a plugin behaves the same everywhere.
//! - [`resolve`] joins the checked segments onto the canonical root, canonicalizes the result
//!   (following symlinks) and requires it to stay under the canonical root, which also refuses
//!   symlinks that lead out of it.
//!
//! Both sides of the containment check come from [`std::fs::canonicalize`], so on Windows both
//! are verbatim (`\\?\`) paths and compare consistently.

use std::path::{Path, PathBuf};

use nugget_core::{Error, Result};

/// Longest relative path accepted, in bytes.
pub const MAX_RELATIVE_PATH_BYTES: usize = 4096;
/// Longest segment (file or folder name) accepted, in bytes.
pub const MAX_SEGMENT_BYTES: usize = 255;
/// Most segments in a relative path.
pub const MAX_SEGMENTS: usize = 128;

/// Names Windows maps to devices in any folder (with or without an extension).
const WINDOWS_DEVICES: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "com0", "com1", "com2", "com3", "com4",
    "com5", "com6", "com7", "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6",
    "lpt7", "lpt8", "lpt9", "com¹", "com²", "com³", "lpt¹", "lpt²", "lpt³",
];

fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

/// Check the syntax of a relative path and return its segments. `""` is the root itself (no
/// segments). Never touches the filesystem.
pub fn check_relative(rel: &str) -> Result<Vec<&str>> {
    if rel.is_empty() {
        return Ok(Vec::new());
    }
    if rel.len() > MAX_RELATIVE_PATH_BYTES {
        return Err(invalid(format!(
            "path is longer than {MAX_RELATIVE_PATH_BYTES} bytes"
        )));
    }
    if rel.starts_with('/') {
        return Err(invalid("path must be relative to the root"));
    }
    let segments: Vec<&str> = rel.split('/').collect();
    if segments.len() > MAX_SEGMENTS {
        return Err(invalid(format!(
            "path has more than {MAX_SEGMENTS} segments"
        )));
    }
    for seg in &segments {
        check_segment(seg)?;
    }
    Ok(segments)
}

/// Whether `name` is one segment [`check_relative`] accepts (a plain file or folder name).
pub fn is_valid_segment(name: &str) -> bool {
    check_segment(name).is_ok()
}

fn check_segment(seg: &str) -> Result<()> {
    match seg {
        "" => {
            return Err(invalid(
                "path has an empty segment (`//` or a trailing `/`)",
            ));
        }
        "." | ".." => return Err(invalid("path may not contain `.` or `..` segments")),
        _ => {}
    }
    if seg.len() > MAX_SEGMENT_BYTES {
        return Err(invalid(format!(
            "path segment is longer than {MAX_SEGMENT_BYTES} bytes"
        )));
    }
    if let Some(c) = seg
        .chars()
        .find(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
    {
        return Err(invalid(format!(
            "path may not contain {c:?} (use `/` between segments; no drive letters)"
        )));
    }
    // Windows ignores trailing dots and spaces and maps device names in every folder; refuse
    // them everywhere so a path means the same file on every OS.
    if seg.ends_with('.') || seg.ends_with(' ') {
        return Err(invalid("path segments may not end with `.` or a space"));
    }
    let stem = seg
        .split('.')
        .next()
        .unwrap_or(seg)
        .trim_end()
        .to_lowercase();
    if WINDOWS_DEVICES.contains(&stem.as_str()) {
        return Err(invalid(format!("`{seg}` is a reserved device name")));
    }
    Ok(())
}

/// Check a root path (a value from the source settings): it must be absolute. Returns its
/// canonical form (symlinks resolved).
pub fn canonical_root(path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.contains('\0') {
        return Err(invalid("the folder or file path is empty or contains NUL"));
    }
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(invalid(format!(
            "the folder or file path `{path}` must be absolute"
        )));
    }
    canonicalize(p)
}

/// Resolve `rel` (see [`check_relative`]) under `root` and return the canonical path, which is
/// `root` itself or below it. `root` is canonicalized first (a no-op for a path from
/// [`canonical_root`]).
///
/// Errors: [`Error::Invalid`] for bad syntax or a relative root, [`Error::NotFound`] when the
/// path does not exist, and [`Error::Unauthorized`] when it resolves (through a symlink) to
/// somewhere outside the root.
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    let segments = check_relative(rel)?;
    if !root.is_absolute() {
        return Err(invalid("the root must be an absolute path"));
    }
    let root = canonicalize(root)?;
    if segments.is_empty() {
        return Ok(root);
    }
    let mut joined = root.clone();
    for seg in segments {
        joined.push(seg);
    }
    let canon = canonicalize(&joined)?;
    if !is_within(&root, &canon) {
        return Err(Error::Unauthorized(format!(
            "`{rel}` leads outside the folder the user chose"
        )));
    }
    Ok(canon)
}

/// Whether canonical `path` is canonical `root` or below it (component-wise).
pub fn is_within(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

fn canonicalize(p: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(p).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::NotFound("no such file or folder".into()),
        // Joining onto a file (`root` is a file, or a segment is one).
        std::io::ErrorKind::NotADirectory => Error::NotFound("no such file or folder".into()),
        _ => Error::Io(e),
    })
}
