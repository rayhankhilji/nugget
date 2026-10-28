//! Safe extraction of runtime archives (`.zip`, `.tar.gz`).
//!
//! Archives are treated as hostile even after their SHA-256 matched the signed manifest:
//! - entry names must be relative, `/`-separated paths of normal components: no absolute paths,
//!   drive letters, `..`, backslashes, colons (NTFS streams), NUL or control characters, and no
//!   Windows device names;
//! - only regular files, directories and symlinks are accepted (no hard links, devices, FIFOs);
//! - a symlink target must be a relative path of normal components (no `..`, no absolute path), so
//!   it can only point at or below its own directory. Symlinks are created **after** every file,
//!   so no file is ever written through one. Where symlinks are unavailable (Windows), the target
//!   file is copied instead;
//! - caps on entry count, per-file size and total size, enforced on the bytes actually
//!   decompressed (headers are not trusted), stop decompression bombs;
//! - files are created with `create_new` (duplicates fail), with mode 0755 or 0644 on Unix (never
//!   setuid/setgid or world-writable);
//! - everything is extracted into a fresh staging directory next to the destination, which is
//!   renamed into place only when extraction succeeded; on failure the staging dir is removed.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runtime::ArchiveFormat;

/// Resource caps for [`extract_archive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractLimits {
    /// Maximum number of entries (files, directories and symlinks).
    pub max_entries: usize,
    /// Maximum size of one extracted file in bytes.
    pub max_file_bytes: u64,
    /// Maximum total extracted bytes.
    pub max_total_bytes: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_file_bytes: 2 * 1024 * 1024 * 1024,
            max_total_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}

/// One extracted regular file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedFile {
    /// Relative `/`-separated path.
    pub path: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the extracted bytes.
    pub sha256: String,
    pub executable: bool,
}

/// What [`extract_archive`] wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractReport {
    pub files: Vec<ExtractedFile>,
    /// `(link path, target)` pairs, both relative.
    pub symlinks: Vec<(String, String)>,
    pub dirs: usize,
    pub total_bytes: u64,
}

impl ExtractReport {
    pub fn file(&self, path: &str) -> Option<&ExtractedFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// Extract `archive` into `dest`, which must not exist yet. See the module docs for the rules.
///
/// Blocking; run it on a blocking thread.
pub fn extract_archive(
    archive: &Path,
    format: ArchiveFormat,
    dest: &Path,
    limits: &ExtractLimits,
) -> Result<ExtractReport> {
    if dest.exists() {
        return Err(Error::Invalid(format!(
            "extraction target {} already exists",
            dest.display()
        )));
    }
    let parent = dest
        .parent()
        .ok_or_else(|| Error::Invalid("extraction target has no parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let name = dest
        .file_name()
        .ok_or_else(|| Error::Invalid("extraction target has no name".into()))?
        .to_string_lossy()
        .into_owned();
    let staging = parent.join(format!(
        ".{name}.partial-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir(&staging)?;

    let res = (|| {
        let mut x = Extractor::new(&staging, *limits);
        match format {
            ArchiveFormat::TarGz => extract_tar_gz(archive, &mut x)?,
            ArchiveFormat::Zip => extract_zip(archive, &mut x)?,
        }
        x.finish()
    })();
    match res {
        Ok(report) => {
            if let Err(e) = std::fs::rename(&staging, dest) {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e.into());
            }
            crate::store::sync_dir(parent);
            Ok(report)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(e)
        }
    }
}

fn hostile(msg: impl Into<String>) -> Error {
    Error::Integrity(format!("unsafe archive: {}", msg.into()))
}

/// Normalise an entry name. `Ok(None)` for the archive root (`./`).
fn sanitize_name(raw: &str) -> Result<Option<String>> {
    if raw.contains(['\\', ':', '\0']) || raw.chars().any(char::is_control) {
        return Err(hostile(format!(
            "entry name {raw:?} has forbidden characters"
        )));
    }
    if raw.starts_with('/') {
        return Err(hostile(format!("absolute entry path {raw:?}")));
    }
    let mut parts = Vec::new();
    for c in raw.split('/') {
        match c {
            "" | "." => continue,
            ".." => return Err(hostile(format!("path traversal in {raw:?}"))),
            c if is_reserved_component(c) => {
                return Err(hostile(format!("reserved name in {raw:?}")));
            }
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    let joined = parts.join("/");
    if joined.len() > 1024 {
        return Err(hostile("entry name too long"));
    }
    Ok(Some(joined))
}

/// Windows device names and names Windows silently rewrites (trailing dot or space).
fn is_reserved_component(c: &str) -> bool {
    if c.ends_with(['.', ' ']) {
        return true;
    }
    let stem = c.split('.').next().unwrap_or(c).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
}

/// A symlink target must be relative and descend only (normal components).
fn check_link_target(link: &str, target: &str) -> Result<()> {
    let ok = !target.is_empty()
        && target.len() <= 1024
        && !target.starts_with('/')
        && !target.contains(['\\', ':', '\0'])
        && !target.chars().any(char::is_control)
        && target
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..");
    if ok {
        Ok(())
    } else {
        Err(hostile(format!(
            "symlink {link:?} points outside its directory ({target:?})"
        )))
    }
}

struct Extractor<'a> {
    root: &'a Path,
    limits: ExtractLimits,
    entries: usize,
    seen: HashSet<String>,
    report: ExtractReport,
}

impl<'a> Extractor<'a> {
    fn new(root: &'a Path, limits: ExtractLimits) -> Self {
        Self {
            root,
            limits,
            entries: 0,
            seen: HashSet::new(),
            report: ExtractReport::default(),
        }
    }

    fn count(&mut self, name: &str) -> Result<()> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(hostile(format!(
                "more than {} entries",
                self.limits.max_entries
            )));
        }
        // Case-insensitive: two names that collide on Windows/macOS are rejected everywhere.
        if !self.seen.insert(name.to_ascii_lowercase()) {
            return Err(hostile(format!("duplicate entry {name:?}")));
        }
        Ok(())
    }

    fn target(&self, rel: &str) -> PathBuf {
        rel.split('/')
            .fold(self.root.to_path_buf(), |p, c| p.join(c))
    }

    fn dir(&mut self, raw: &str) -> Result<()> {
        let Some(rel) = sanitize_name(raw)? else {
            return Ok(());
        };
        self.count(&rel)?;
        std::fs::create_dir_all(self.target(&rel))?;
        self.report.dirs += 1;
        Ok(())
    }

    fn file(
        &mut self,
        raw: &str,
        declared: u64,
        mode: Option<u32>,
        data: &mut dyn Read,
    ) -> Result<()> {
        let rel = sanitize_name(raw)?.ok_or_else(|| hostile("file entry without a name"))?;
        self.count(&rel)?;
        if declared > self.limits.max_file_bytes {
            return Err(hostile(format!("{rel:?} declares {declared} bytes")));
        }
        let path = self.target(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let executable = mode.is_some_and(|m| m & 0o111 != 0);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(if executable { 0o755 } else { 0o644 });
        }
        let mut out = opts
            .open(&path)
            .map_err(|e| hostile(format!("cannot create {rel:?}: {e}")))?;
        let budget = self
            .limits
            .max_file_bytes
            .min(self.limits.max_total_bytes - self.report.total_bytes);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut written = 0u64;
        loop {
            let n = match data.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(hostile(format!("corrupt data in {rel:?}: {e}"))),
            };
            written += n as u64;
            if written > budget {
                return Err(hostile(format!(
                    "{rel:?} exceeds the size limit (decompression bomb?)"
                )));
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
        }
        out.flush()?;
        self.report.total_bytes += written;
        self.report.files.push(ExtractedFile {
            path: rel,
            size: written,
            sha256: hex::encode(hasher.finalize()),
            executable,
        });
        Ok(())
    }

    fn symlink(&mut self, raw: &str, target: &str) -> Result<()> {
        let rel = sanitize_name(raw)?.ok_or_else(|| hostile("symlink entry without a name"))?;
        self.count(&rel)?;
        check_link_target(&rel, target)?;
        self.report.symlinks.push((rel, target.to_string()));
        Ok(())
    }

    /// Create the deferred symlinks, then return the report.
    fn finish(mut self) -> Result<ExtractReport> {
        let links = std::mem::take(&mut self.report.symlinks);
        // Resolve chains (a -> b -> file) where symlinks are copied: retry until no progress.
        let mut pending: Vec<&(String, String)> = links.iter().collect();
        while !pending.is_empty() {
            let before = pending.len();
            let mut next = Vec::new();
            for link in pending {
                if !self.make_link(&link.0, &link.1)? {
                    next.push(link);
                }
            }
            if next.len() == before {
                // Dangling links on platforms that copy: skip them (nothing points there).
                for (l, t) in next {
                    tracing::warn!(link = %l, target = %t, "skipping dangling symlink");
                }
                break;
            }
            pending = next;
        }
        self.report.symlinks = links;
        Ok(self.report)
    }

    /// `Ok(false)` if the target does not exist yet (copy mode only).
    fn make_link(&self, rel: &str, target: &str) -> Result<bool> {
        let link = self.target(rel);
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, &link)
                .map_err(|e| hostile(format!("cannot create symlink {rel:?}: {e}")))?;
            Ok(true)
        }
        #[cfg(not(unix))]
        {
            let parent = link.parent().unwrap_or(self.root);
            let src = target
                .split('/')
                .fold(parent.to_path_buf(), |p, c| p.join(c));
            match std::fs::symlink_metadata(&src) {
                Ok(md) if md.is_file() => {
                    std::fs::copy(&src, &link)?;
                    Ok(true)
                }
                Ok(_) => Ok(true), // a directory link: not needed by any runtime; skip
                Err(_) => Ok(false),
            }
        }
    }
}

/// Reader that fails once more than `cap` bytes have been read (bounds the decompressed stream,
/// headers and padding included).
struct CappedReader<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for CappedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.left = self
            .left
            .checked_sub(n as u64)
            .ok_or_else(|| std::io::Error::other("decompressed archive exceeds the size limit"))?;
        Ok(n)
    }
}

fn extract_tar_gz(archive: &Path, x: &mut Extractor<'_>) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    // Headers and padding: at most ~1.5 KiB per entry on top of the data, plus slack.
    let overhead = (x.limits.max_entries as u64 + 16).saturating_mul(3 * 512) + 64 * 1024;
    let reader = CappedReader {
        inner: flate2::read::GzDecoder::new(std::io::BufReader::new(file)),
        left: x.limits.max_total_bytes.saturating_add(overhead),
    };
    let mut ar = tar::Archive::new(reader);
    let entries = ar
        .entries()
        .map_err(|e| hostile(format!("not a tar archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| hostile(format!("corrupt tar: {e}")))?;
        let name = String::from_utf8(entry.path_bytes().into_owned())
            .map_err(|_| hostile("entry name is not UTF-8"))?;
        let kind = entry.header().entry_type();
        use tar::EntryType as T;
        match kind {
            T::Regular | T::Continuous => {
                let mode = entry.header().mode().ok();
                let size = entry.header().size().unwrap_or(u64::MAX);
                x.file(&name, size, mode, &mut entry)?;
            }
            T::Directory => x.dir(&name)?,
            T::Symlink => {
                let target = entry
                    .link_name_bytes()
                    .ok_or_else(|| hostile(format!("symlink {name:?} has no target")))?;
                let target = String::from_utf8(target.into_owned())
                    .map_err(|_| hostile("symlink target is not UTF-8"))?;
                x.symlink(&name, &target)?;
            }
            T::XGlobalHeader | T::XHeader => {}
            other => {
                return Err(hostile(format!(
                    "entry {name:?} has unsupported type {other:?} (hard link, device or FIFO)"
                )));
            }
        }
    }
    Ok(())
}

fn extract_zip(archive: &Path, x: &mut Extractor<'_>) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| hostile(format!("not a zip archive: {e}")))?;
    if zip.len() > x.limits.max_entries {
        return Err(hostile(format!(
            "{} entries (limit {})",
            zip.len(),
            x.limits.max_entries
        )));
    }
    for i in 0..zip.len() {
        let mut f = zip
            .by_index(i)
            .map_err(|e| hostile(format!("corrupt zip entry {i}: {e}")))?;
        let name = f
            .name()
            .map_err(|e| hostile(format!("bad zip entry name {i}: {e}")))?
            .into_owned();
        let mode = f.unix_mode();
        if f.is_symlink() {
            let mut target = String::new();
            (&mut f)
                .take(1025)
                .read_to_string(&mut target)
                .map_err(|_| hostile(format!("bad symlink target in {name:?}")))?;
            x.symlink(&name, &target)?;
        } else if f.is_dir() {
            x.dir(&name)?;
        } else if f.is_file() {
            let size = f.size();
            x.file(&name, size, mode, &mut f)?;
        } else {
            return Err(hostile(format!("entry {name:?} has an unsupported type")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_sanitising() {
        assert_eq!(sanitize_name("./a/b").unwrap().as_deref(), Some("a/b"));
        assert_eq!(sanitize_name("a//b/").unwrap().as_deref(), Some("a/b"));
        assert_eq!(sanitize_name("./").unwrap(), None);
        for bad in [
            "../x",
            "a/../../x",
            "/etc/passwd",
            "C:/x",
            "a\\b",
            "a\0b",
            "x/CON",
            "nul.txt",
            "lpt1",
            "dot.",
            "space ",
            "file:stream",
        ] {
            assert!(sanitize_name(bad).is_err(), "{bad}");
        }
        assert!(sanitize_name("console.txt").unwrap().is_some());
        assert!(sanitize_name("com10").unwrap().is_some());
    }

    #[test]
    fn link_targets() {
        check_link_target("a", "libx.so.1").unwrap();
        check_link_target("a", "sub/libx.so").unwrap();
        for bad in [
            "../x",
            "/etc/passwd",
            "a/../../b",
            ".",
            "",
            "C:\\x",
            "a/./b",
        ] {
            assert!(check_link_target("l", bad).is_err(), "{bad}");
        }
    }
}
