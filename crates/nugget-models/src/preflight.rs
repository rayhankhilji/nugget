//! Fail-closed disk-space preflight.

use std::path::Path;

use nugget_core::{Error, Result};

/// One GiB.
pub const GIB: u64 = 1024 * 1024 * 1024;

/// Source of free-space information. Injectable so tests can simulate full or unreadable disks.
pub trait DiskSpace: Send + Sync {
    /// Bytes available to the current (non-privileged) user on the filesystem holding `path`.
    fn available_bytes(&self, path: &Path) -> std::io::Result<u64>;
}

/// Real filesystem query (`statvfs` on Unix, `GetDiskFreeSpaceExW` on Windows).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDiskSpace;

impl DiskSpace for SystemDiskSpace {
    fn available_bytes(&self, path: &Path) -> std::io::Result<u64> {
        fs4::available_space(path)
    }
}

/// Bytes that must be free before downloading a `size`-byte model of which `already_staged`
/// bytes are already on disk: the remainder, plus 10% of the full size, plus 1 GiB headroom.
pub fn required_bytes(size: u64, already_staged: u64) -> u64 {
    size.saturating_sub(already_staged)
        .saturating_add(size / 10)
        .saturating_add(GIB)
}

/// Check that `dir` has room for a `size`-byte model.
///
/// Too little room is [`Error::InsufficientSpace`]. **Fails closed**: if free space cannot be
/// determined, that is an [`Error::Storage`] (never a pass, and never an `InsufficientSpace`
/// with a made-up `available`).
pub fn check_disk_space(
    disk: &dyn DiskSpace,
    dir: &Path,
    size: u64,
    already_staged: u64,
) -> Result<()> {
    let needed = required_bytes(size, already_staged);
    let available = disk.available_bytes(dir).map_err(|e| {
        Error::Storage(format!(
            "cannot determine free disk space at {} ({e}); refusing to download",
            dir.display()
        ))
    })?;
    if available < needed {
        tracing::warn!(
            dir = %dir.display(),
            needed,
            available,
            "insufficient disk space (model + 10% + 1 GiB)"
        );
        return Err(Error::InsufficientSpace { needed, available });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirement_is_size_plus_ten_percent_plus_gib() {
        assert_eq!(required_bytes(10 * GIB, 0), 10 * GIB + GIB + GIB);
        // Already-staged bytes are not counted twice.
        assert_eq!(required_bytes(10 * GIB, 4 * GIB), 6 * GIB + GIB + GIB);
        assert_eq!(required_bytes(u64::MAX, 0), u64::MAX);
    }
}
