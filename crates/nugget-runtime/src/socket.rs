//! The private directory for the servers' Unix sockets (Unix only).
//!
//! `llama-server --host <path>.sock` listens on a Unix-domain socket instead of TCP. The socket
//! lives in a directory that only the current user can enter (mode 0700, owned by us), so no
//! other local user can connect to the server or plant a socket of their own at its path: the
//! port race of a loopback TCP listener (see [`crate::listener`]) does not exist.
#![cfg_attr(not(unix), allow(dead_code))]

use std::path::{Path, PathBuf};

use nugget_core::paths::Paths;
use nugget_core::{Error, Result};

/// `sun_path` holds 104 bytes on macOS and 108 on Linux, including the NUL; keep a margin.
const MAX_SOCKET_PATH: usize = 100;

/// The longest socket file name used in the directory.
const LONGEST_NAME: &str = "embedding.sock";

/// Socket file of `role_name`'s server inside `dir`.
pub(crate) fn socket_path(dir: &Path, role_name: &str) -> PathBuf {
    dir.join(format!("{role_name}.sock"))
}

/// Where the sockets go: `<data>/run`, or, when that path is too long for a socket address, a
/// short per-installation directory in `$XDG_RUNTIME_DIR` (if it is private) or the temp dir.
pub(crate) fn socket_dir_in(paths: &Paths, runtime: Option<PathBuf>) -> PathBuf {
    let fits = |d: &Path| d.join(LONGEST_NAME).as_os_str().len() <= MAX_SOCKET_PATH;
    let preferred = paths.data.join("run");
    if fits(&preferred) {
        return preferred;
    }
    let leaf = format!(
        "nugget-run-{:016x}",
        fnv1a(paths.data.as_os_str().as_encoded_bytes())
    );
    if let Some(dir) = runtime.map(|r| r.join(&leaf))
        && fits(&dir)
    {
        return dir;
    }
    std::env::temp_dir().join(leaf)
}

/// FNV-1a 64: stable across platforms and releases.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Create (or check) the private socket directory and return it.
///
/// The directory must be a real directory (not a symlink) owned by the owner of the data
/// directory (us; read without libc) with no group or other permissions. One of ours that is
/// too open is tightened to 0700; anything else is refused, since a directory another user
/// controls could hold their socket.
#[cfg(unix)]
pub(crate) fn private_dir(paths: &Paths) -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let io = |what: &str, p: &Path, e: std::io::Error| {
        Error::Storage(format!("{what} {}: {e}", p.display()))
    };
    std::fs::create_dir_all(&paths.data).map_err(|e| io("cannot create", &paths.data, e))?;
    let uid = std::fs::metadata(&paths.data)
        .map_err(|e| io("cannot read", &paths.data, e))?
        .uid();
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| {
            d.is_absolute()
                && std::fs::symlink_metadata(d)
                    .is_ok_and(|m| m.is_dir() && m.uid() == uid && m.mode() & 0o777 == 0o700)
        });
    let dir = socket_dir_in(paths, runtime);
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io("cannot create", &dir, e)),
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| io("cannot read", &dir, e))?;
    if !meta.is_dir() || meta.uid() != uid {
        return Err(Error::Storage(format!(
            "{} is not a directory owned by the current user; refusing to put the runtime's \
             sockets there",
            dir.display()
        )));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| io("cannot restrict", &dir, e))?;
    }
    Ok(dir)
}

/// Remove a socket file left behind by an earlier server (no-op if there is none).
pub(crate) fn remove_stale(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "cannot remove runtime socket");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_data_dirs_hold_the_sockets_and_long_ones_fall_back() {
        let paths = Paths::rooted(PathBuf::from("/home/u/.local/share/nugget"));
        assert_eq!(
            socket_dir_in(&paths, None),
            PathBuf::from("/home/u/.local/share/nugget/data/run")
        );
        let deep = Paths::rooted(PathBuf::from(format!("/home/u/{}", "x".repeat(120))));
        let rt = PathBuf::from("/run/user/1000");
        let d = socket_dir_in(&deep, Some(rt.clone()));
        assert!(d.starts_with(&rt), "{}", d.display());
        assert!(socket_path(&d, "embedding").as_os_str().len() <= MAX_SOCKET_PATH);
        // Stable per installation.
        assert_eq!(d, socket_dir_in(&deep, Some(rt)));
        let t = socket_dir_in(&deep, None);
        assert!(t.starts_with(std::env::temp_dir()), "{}", t.display());
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path().to_path_buf());
        let dir = private_dir(&paths).unwrap();
        assert_eq!(dir, paths.data.join("run"));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        // Loosened by somebody (ourselves): tightened again.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(private_dir(&paths).unwrap(), dir);
        assert_eq!(mode(&dir), 0o700);
        // A symlink in its place is refused.
        std::fs::remove_dir(&dir).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        assert!(private_dir(&paths).is_err());
    }
}
