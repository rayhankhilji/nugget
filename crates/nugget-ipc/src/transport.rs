//! Where the daemon listens: a Unix socket inside a user-private (0700) directory on
//! macOS/Linux, a per-user named pipe on Windows.

use std::io;

use interprocess::local_socket::Name;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::traits::tokio::Stream as _;
use nugget_core::ipc::SOCKET_NAME;
use nugget_core::paths::Paths;

/// The local socket name for the daemon described by `paths`.
///
/// Pure: does not touch the filesystem. Use [`bind`] on the server side, which also prepares the
/// socket directory.
pub fn socket_name(paths: &Paths) -> io::Result<Name<'static>> {
    imp::name(paths)
}

/// Human-readable socket location (a path on Unix, a pipe name on Windows), for logs and UI.
pub fn socket_display(paths: &Paths) -> String {
    imp::display(paths)
}

/// Bind the daemon's listener.
///
/// On Unix this creates the socket directory with mode 0700 (and refuses to use one that is
/// not private to the current user). A stale socket file left by a crashed daemon is
/// replaced; if another daemon is still answering on it, this fails with
/// [`io::ErrorKind::AddrInUse`].
pub async fn bind(paths: &Paths) -> io::Result<Listener> {
    imp::bind(paths).await
}

/// Connect a raw stream to the daemon. Most callers want [`Client::connect`](crate::Client::connect).
pub async fn connect_stream(paths: &Paths) -> io::Result<Stream> {
    imp::check_before_connect(paths)?;
    Stream::connect(socket_name(paths)?).await
}

/// FNV-1a 64; stable across platforms and releases (unlike `DefaultHasher`).
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(unix)]
mod imp {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    use interprocess::local_socket::tokio::{Listener, Stream};
    use interprocess::local_socket::traits::tokio::Stream as _;
    use interprocess::local_socket::{GenericFilePath, ListenerOptions, Name, ToFsName};

    use super::{SOCKET_NAME, fnv1a};
    use nugget_core::paths::Paths;

    /// `sun_path` is 104 bytes on macOS and 108 on Linux, including the NUL.
    const MAX_SOCKET_PATH: usize = 100;

    pub(super) fn socket_dir(paths: &Paths) -> PathBuf {
        let preferred = paths.data.join("ipc");
        if preferred.join(SOCKET_NAME).as_os_str().len() <= MAX_SOCKET_PATH {
            return preferred;
        }
        // Deep home directories: fall back to a short per-installation directory. It is
        // ownership- and mode-checked before use because the temp dir may be shared.
        let hash = fnv1a(paths.data.as_os_str().as_encoded_bytes());
        std::env::temp_dir().join(format!("nugget-{hash:016x}"))
    }

    fn socket_path(paths: &Paths) -> PathBuf {
        socket_dir(paths).join(SOCKET_NAME)
    }

    pub(super) fn name(paths: &Paths) -> io::Result<Name<'static>> {
        socket_path(paths).to_fs_name::<GenericFilePath>()
    }

    pub(super) fn display(paths: &Paths) -> String {
        socket_path(paths).display().to_string()
    }

    /// Owner uid of a directory we created ourselves, used as "our" uid without libc.
    fn our_uid(paths: &Paths) -> io::Result<u32> {
        fs::create_dir_all(&paths.data)?;
        Ok(fs::metadata(&paths.data)?.uid())
    }

    fn ensure_private_dir(paths: &Paths) -> io::Result<PathBuf> {
        let dir = socket_dir(paths);
        let uid = our_uid(paths)?;
        match fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = fs::symlink_metadata(&dir)?;
        if !meta.is_dir() || meta.uid() != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is not a directory owned by the current user",
                    dir.display()
                ),
            ));
        }
        if meta.mode() & 0o077 != 0 {
            // Ours, but too open (e.g. created under a permissive umask by an older build).
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(dir)
    }

    pub(super) fn check_before_connect(paths: &Paths) -> io::Result<()> {
        // Only the shared-temp fallback can be planted by another user; the preferred
        // directory lives under the user's own data dir.
        let dir = socket_dir(paths);
        if dir.starts_with(&paths.data) {
            return Ok(());
        }
        let meta = fs::symlink_metadata(&dir)?;
        let uid = fs::metadata(&paths.data)?.uid();
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("refusing to connect: {} is not private", dir.display()),
            ));
        }
        Ok(())
    }

    fn listen(path: &Path) -> io::Result<Listener> {
        ListenerOptions::new()
            .name(path.to_path_buf().to_fs_name::<GenericFilePath>()?)
            .reclaim_name(true)
            .create_tokio()
    }

    pub(super) async fn bind(paths: &Paths) -> io::Result<Listener> {
        let dir = ensure_private_dir(paths)?;
        let path = dir.join(SOCKET_NAME);
        match listen(&path) {
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                let name = path.clone().to_fs_name::<GenericFilePath>()?;
                if Stream::connect(name).await.is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("nuggetd is already listening on {}", path.display()),
                    ));
                }
                tracing::info!(path = %path.display(), "removing stale socket");
                fs::remove_file(&path)?;
                listen(&path)
            }
            other => other,
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;

    use interprocess::local_socket::tokio::Listener;
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Name, ToNsName};
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;

    use super::{SOCKET_NAME, fnv1a};
    use nugget_core::paths::Paths;

    /// Protected DACL: full access for LocalSystem and the pipe's owner (the user running
    /// nuggetd) only. Everyone else, including other interactive users, is denied.
    const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;OW)";

    fn pipe_name(paths: &Paths) -> String {
        let user: String = std::env::var("USERNAME")
            .unwrap_or_else(|_| "user".into())
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        // The pipe namespace is machine-wide: include the user and the data dir so separate
        // users and separate NUGGET_HOMEs never collide.
        let hash = fnv1a(paths.data.as_os_str().as_encoded_bytes());
        format!("nugget-{user}-{hash:016x}-{SOCKET_NAME}")
    }

    pub(super) fn name(paths: &Paths) -> io::Result<Name<'static>> {
        pipe_name(paths).to_ns_name::<GenericNamespaced>()
    }

    pub(super) fn display(paths: &Paths) -> String {
        format!(r"\\.\pipe\{}", pipe_name(paths))
    }

    pub(super) fn check_before_connect(_paths: &Paths) -> io::Result<()> {
        Ok(())
    }

    pub(super) async fn bind(paths: &Paths) -> io::Result<Listener> {
        let sddl = widestring::U16CString::from_str(PIPE_SDDL)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let sd = SecurityDescriptor::deserialize(&sddl)?;
        ListenerOptions::new()
            .name(name(paths)?)
            .security_descriptor(sd)
            .create_tokio()
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::io;

    use interprocess::local_socket::tokio::Listener;
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Name, ToNsName};

    use super::SOCKET_NAME;
    use nugget_core::paths::Paths;

    pub(super) fn name(_paths: &Paths) -> io::Result<Name<'static>> {
        SOCKET_NAME.to_ns_name::<GenericNamespaced>()
    }

    pub(super) fn display(_paths: &Paths) -> String {
        SOCKET_NAME.to_owned()
    }

    pub(super) fn check_before_connect(_paths: &Paths) -> io::Result<()> {
        Ok(())
    }

    pub(super) async fn bind(paths: &Paths) -> io::Result<Listener> {
        ListenerOptions::new().name(name(paths)?).create_tokio()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn long_data_dirs_fall_back_to_short_socket_paths() {
        let paths = Paths::rooted(std::path::PathBuf::from("/").join("x".repeat(200)));
        let dir = imp::socket_dir(&paths);
        assert!(dir.join(SOCKET_NAME).as_os_str().len() <= 100);
        assert!(dir.starts_with(std::env::temp_dir()));
    }

    #[tokio::test]
    async fn bind_creates_private_dir_and_fixes_loose_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path().to_path_buf());
        let dir = imp::socket_dir(&paths);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _listener = bind(&paths).await.unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(socket_display(&paths).ends_with(SOCKET_NAME));
    }
}
