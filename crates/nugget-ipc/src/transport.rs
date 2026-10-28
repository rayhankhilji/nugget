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
/// First takes an exclusive lock on `<data>/ipc/nuggetd.lock` (see [`LOCK_FILE`]), before the
/// socket or pipe is touched, so two daemons starting at once cannot both claim the name; if
/// another process holds it, this fails with [`io::ErrorKind::AddrInUse`]. The lock is kept
/// until the process exits ([`Listener`] cannot carry it); a later `bind` in the same process
/// reuses it once the earlier listener is gone.
///
/// On Unix this creates the socket directory with mode 0700 (and refuses to use one that is
/// not private to the current user). A stale socket file left by a crashed daemon is
/// replaced; if another daemon is still answering on it, this fails with
/// [`io::ErrorKind::AddrInUse`].
pub async fn bind(paths: &Paths) -> io::Result<Listener> {
    let lock = daemon_lock::acquire(paths)?;
    let listener = imp::bind(paths).await?;
    daemon_lock::retain(lock);
    Ok(listener)
}

/// Name of the daemon lock file inside `<data>/ipc/`.
pub const LOCK_FILE: &str = "nuggetd.lock";

/// Path of the lock file [`bind`] holds while a daemon runs.
pub fn lock_path(paths: &Paths) -> std::path::PathBuf {
    paths.data.join("ipc").join(LOCK_FILE)
}

/// Connect a raw stream to the daemon. Most callers want [`Client::connect`](crate::Client::connect).
///
/// On Windows the pipe server is verified to run as the current user before the stream is
/// returned (another account could have created a pipe with the expected name).
pub async fn connect_stream(paths: &Paths) -> io::Result<Stream> {
    imp::check_before_connect(paths)?;
    let stream = Stream::connect(socket_name(paths)?).await?;
    imp::verify_server(&stream)?;
    Ok(stream)
}

/// Server side: refuse an accepted connection whose peer is not the current user.
///
/// On Unix the peer's effective uid (`SO_PEERCRED` on Linux, `LOCAL_PEERCRED`, which backs
/// `getpeereid`, on macOS/BSD) must equal ours; this backs up the 0700 socket directory. On
/// Windows the pipe's DACL already admits only the owner and LocalSystem, so this accepts.
pub(crate) fn verify_client(stream: &Stream) -> io::Result<()> {
    imp::verify_client(stream)
}

/// The single-daemon lock: an exclusive, non-blocking `fs4` lock on [`lock_path`].
#[cfg(any(unix, windows))]
mod daemon_lock {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Mutex, PoisonError};

    use nugget_core::paths::Paths;

    /// Locks this process holds, by lock path. Never released before exit: [`super::Listener`]
    /// is a foreign type, so the lock cannot live inside it.
    static HELD: Mutex<Vec<(PathBuf, File)>> = Mutex::new(Vec::new());

    /// A newly taken lock, or `None` if this process already holds it.
    pub(super) struct Acquired(Option<(PathBuf, File)>);

    pub(super) fn acquire(paths: &Paths) -> io::Result<Acquired> {
        let path = super::lock_path(paths);
        if let Some(dir) = path.parent() {
            create_lock_dir(dir)?;
        }
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        // A fresh handle every time: locks belong to the open file, so this conflicts with any
        // other holder, in this process or another.
        let file = opts.open(&path)?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => Ok(Acquired(Some((path, file)))),
            Err(fs4::TryLockError::WouldBlock) => {
                let held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
                if held.iter().any(|(p, _)| *p == path) {
                    // Ours, from an earlier bind; the socket check decides whether that
                    // listener is still alive.
                    Ok(Acquired(None))
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!(
                            "another nuggetd is starting or running (it holds {})",
                            path.display()
                        ),
                    ))
                }
            }
            Err(fs4::TryLockError::Error(e)) => Err(e),
        }
    }

    /// Keep a newly taken lock for the rest of the process.
    pub(super) fn retain(lock: Acquired) {
        if let Some(entry) = lock.0 {
            HELD.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(entry);
        }
    }

    #[cfg(unix)]
    fn create_lock_dir(dir: &std::path::Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }

    #[cfg(windows)]
    fn create_lock_dir(dir: &std::path::Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)
    }
}

#[cfg(not(any(unix, windows)))]
mod daemon_lock {
    use std::io;

    use nugget_core::paths::Paths;

    pub(super) struct Acquired;

    pub(super) fn acquire(_paths: &Paths) -> io::Result<Acquired> {
        Ok(Acquired)
    }

    pub(super) fn retain(_lock: Acquired) {}
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
    use interprocess::local_socket::traits::StreamCommon as _;
    use interprocess::local_socket::traits::tokio::Stream as _;
    use interprocess::local_socket::{GenericFilePath, ListenerOptions, Name, ToFsName};

    use super::{SOCKET_NAME, fnv1a};
    use nugget_core::paths::Paths;

    /// `sun_path` is 104 bytes on macOS and 108 on Linux, including the NUL.
    const MAX_SOCKET_PATH: usize = 100;

    pub(super) fn socket_dir(paths: &Paths) -> PathBuf {
        socket_dir_in(
            paths,
            runtime_dir(paths, std::env::var_os("XDG_RUNTIME_DIR")),
        )
    }

    /// `socket_dir` with the runtime directory already checked (see [`runtime_dir`]).
    pub(super) fn socket_dir_in(paths: &Paths, runtime: Option<PathBuf>) -> PathBuf {
        let preferred = paths.data.join("ipc");
        if preferred.join(SOCKET_NAME).as_os_str().len() <= MAX_SOCKET_PATH {
            return preferred;
        }
        // Deep home directories: fall back to a short per-installation directory, preferably
        // in the user's private runtime dir, else in the (possibly shared) temp dir. Either is
        // ownership- and mode-checked before use, since a shared temp dir can be squatted.
        let hash = fnv1a(paths.data.as_os_str().as_encoded_bytes());
        let leaf = format!("nugget-{hash:016x}");
        if let Some(dir) = runtime.map(|r| r.join(&leaf))
            && dir.join(SOCKET_NAME).as_os_str().len() <= MAX_SOCKET_PATH
        {
            return dir;
        }
        std::env::temp_dir().join(leaf)
    }

    /// `$XDG_RUNTIME_DIR` if it is an absolute path to a directory owned by us (the owner of
    /// the data dir) with mode 0700; otherwise `None`.
    pub(super) fn runtime_dir(paths: &Paths, var: Option<std::ffi::OsString>) -> Option<PathBuf> {
        let dir = PathBuf::from(var?);
        if !dir.is_absolute() {
            return None;
        }
        let meta = fs::symlink_metadata(&dir).ok()?;
        let uid = fs::metadata(&paths.data).ok()?.uid();
        (meta.is_dir() && meta.uid() == uid && meta.mode() & 0o777 == 0o700).then_some(dir)
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
        // Creates the data dir first, which `socket_dir` needs to vet $XDG_RUNTIME_DIR.
        let uid = our_uid(paths)?;
        let dir = socket_dir(paths);
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

    pub(super) fn verify_server(_stream: &Stream) -> io::Result<()> {
        // The socket lives in a directory only we can write (checked above / at bind).
        Ok(())
    }

    /// Whether a peer with effective uid `peer` (`None`: the OS did not report one) may talk
    /// to a server running as `ours`. Unknown peers are refused.
    pub(super) fn peer_uid_allowed(peer: Option<u32>, ours: u32) -> bool {
        peer == Some(ours)
    }

    pub(super) fn verify_client(stream: &Stream) -> io::Result<()> {
        let peer = stream.peer_creds()?.euid();
        let ours = rustix::process::geteuid().as_raw();
        if peer_uid_allowed(peer, ours) {
            return Ok(());
        }
        let who = peer.map_or_else(|| "an unknown user".to_owned(), |uid| format!("uid {uid}"));
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("peer runs as {who}, not as the daemon's uid {ours}"),
        ))
    }

    pub(super) fn check_before_connect(paths: &Paths) -> io::Result<()> {
        // Only the fallback can be planted by another user; the preferred
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

    use interprocess::local_socket::tokio::{Listener, Stream};
    use interprocess::local_socket::traits::StreamCommon as _;
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Name, ToNsName};
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;

    use super::{SOCKET_NAME, fnv1a};
    use nugget_core::paths::Paths;

    /// Protected DACL: full access for LocalSystem and the pipe's owner (the user running
    /// nuggetd) only. Everyone else, including other interactive users, is denied.
    const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;OW)";

    fn pipe_name(paths: &Paths) -> io::Result<String> {
        // The pipe namespace is machine-wide: include the user's SID (unlike USERNAME, it is
        // unique and cannot be chosen by another account) and the data dir, so separate users
        // and separate NUGGET_HOMEs never collide. Another account can still create a pipe with
        // this name first, which is why clients verify the server (`verify_server`).
        let sid = super::win::current_user_sid()?;
        let hash = fnv1a(paths.data.as_os_str().as_encoded_bytes());
        Ok(format!("nugget-{sid}-{hash:016x}-{SOCKET_NAME}"))
    }

    pub(super) fn name(paths: &Paths) -> io::Result<Name<'static>> {
        pipe_name(paths)?.to_ns_name::<GenericNamespaced>()
    }

    pub(super) fn display(paths: &Paths) -> String {
        match pipe_name(paths) {
            Ok(n) => format!(r"\\.\pipe\{n}"),
            Err(e) => format!("<pipe name unavailable: {e}>"),
        }
    }

    pub(super) fn check_before_connect(_paths: &Paths) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn verify_client(_stream: &Stream) -> io::Result<()> {
        // `PIPE_SDDL` admits only the owner and LocalSystem.
        Ok(())
    }

    /// Refuse a pipe served by a process that does not run as the current user.
    ///
    /// Limitation: interprocess opens the client end with `CreateFileW` and no
    /// `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` flags (it has no option for them), so
    /// a squatting server could impersonate this client at the default level until this check
    /// disconnects it. Impersonating at that level needs `SeImpersonatePrivilege`, which normal
    /// accounts lack, and nothing is sent before the check passes.
    pub(super) fn verify_server(stream: &Stream) -> io::Result<()> {
        // For a client stream, interprocess answers this with `GetNamedPipeServerProcessId`.
        let pid = stream.peer_creds()?.pid().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "pipe server has no process id",
            )
        })?;
        match super::win::process_is_current_user(pid) {
            Ok(true) => Ok(()),
            Ok(false) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to connect: the nuggetd pipe is served by process {pid}, which runs \
                     as another user"
                ),
            )),
            Err(e) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to connect: cannot verify the user of pipe server process {pid}: {e}"
                ),
            )),
        }
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

/// The few Win32 token/SID calls the pipe checks need.
#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    use std::io;
    use std::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// An owned kernel handle, closed on drop.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a handle we opened and own; it is closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// A `TOKEN_USER` as returned by `GetTokenInformation`, in a `u64` buffer so it is aligned
    /// for the struct. The SID it points to lives inside the same buffer.
    struct TokenUserBuf(Vec<u64>);

    impl TokenUserBuf {
        fn sid(&self) -> PSID {
            // SAFETY: the buffer was filled by GetTokenInformation(TokenUser), so it starts with
            // a valid, aligned TOKEN_USER whose SID pointer targets this (live) buffer.
            unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
        }
    }

    /// The user of the token of `process` (a handle with query access).
    fn token_user(process: HANDLE) -> io::Result<TokenUserBuf> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: `process` is a valid process handle and `token` a valid out pointer.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = Owned(token);
        let mut len = 0u32;
        // SAFETY: a size query: null buffer of length 0, `len` is a valid out pointer. It fails
        // with ERROR_INSUFFICIENT_BUFFER and sets `len`.
        unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` is writable for at least `len` bytes and 8-byte aligned.
        let ok = unsafe {
            GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(TokenUserBuf(buf))
    }

    fn current_token_user() -> io::Result<TokenUserBuf> {
        // SAFETY: GetCurrentProcess returns a pseudo handle that is always valid and is not
        // closed.
        token_user(unsafe { GetCurrentProcess() })
    }

    /// The current user's SID as a string (`S-1-5-21-...`).
    pub(super) fn current_user_sid() -> io::Result<String> {
        let user = current_token_user()?;
        let mut wide: *mut u16 = ptr::null_mut();
        // SAFETY: `user.sid()` is a valid SID kept alive by `user`; `wide` is a valid out pointer.
        if unsafe { ConvertSidToStringSidW(user.sid(), &mut wide) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: on success `wide` is a NUL-terminated UTF-16 string allocated by LocalAlloc;
        // it is read once, then freed exactly once.
        let sid = unsafe { widestring::U16CStr::from_ptr_str(wide) }.to_string_lossy();
        // SAFETY: as above; `wide` is not used afterwards.
        unsafe { LocalFree(wide.cast()) };
        Ok(sid)
    }

    /// Whether process `pid` runs as the current user (token user SIDs are equal).
    pub(super) fn process_is_current_user(pid: u32) -> io::Result<bool> {
        // SAFETY: plain call; a null result is checked and the handle is owned by `Owned`.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let process = Owned(process);
        let theirs = token_user(process.0)?;
        let ours = current_token_user()?;
        // SAFETY: both SIDs are valid and kept alive by `theirs` and `ours`.
        Ok(unsafe { EqualSid(theirs.sid(), ours.sid()) } != 0)
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

    pub(super) fn verify_server(
        _stream: &interprocess::local_socket::tokio::Stream,
    ) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn verify_client(
        _stream: &interprocess::local_socket::tokio::Stream,
    ) -> io::Result<()> {
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
    fn long_data_dirs_prefer_a_private_runtime_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path().join("x".repeat(120)));
        std::fs::create_dir_all(&paths.data).unwrap();
        let rt = tempfile::tempdir().unwrap();
        let set_mode =
            |m| std::fs::set_permissions(rt.path(), std::fs::Permissions::from_mode(m)).unwrap();
        set_mode(0o700);

        let runtime = imp::runtime_dir(&paths, Some(rt.path().into()));
        assert_eq!(runtime.as_deref(), Some(rt.path()));
        let dir = imp::socket_dir_in(&paths, runtime);
        assert!(dir.starts_with(rt.path()), "{}", dir.display());
        assert!(dir.join(SOCKET_NAME).as_os_str().len() <= 100);

        // Unset, relative, missing or not private: the temp-dir scheme is used instead.
        assert_eq!(imp::runtime_dir(&paths, None), None);
        assert_eq!(imp::runtime_dir(&paths, Some("run/user".into())), None);
        assert_eq!(
            imp::runtime_dir(&paths, Some(rt.path().join("missing").into())),
            None
        );
        set_mode(0o755);
        assert_eq!(imp::runtime_dir(&paths, Some(rt.path().into())), None);
        assert!(imp::socket_dir_in(&paths, None).starts_with(std::env::temp_dir()));
    }

    #[test]
    fn only_peers_running_as_our_uid_are_allowed() {
        assert!(imp::peer_uid_allowed(Some(501), 501));
        assert!(!imp::peer_uid_allowed(Some(502), 501));
        assert!(!imp::peer_uid_allowed(Some(0), 501));
        assert!(!imp::peer_uid_allowed(None, 501));
    }

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

#[cfg(all(test, any(unix, windows)))]
mod lock_tests {
    use super::*;

    /// Another handle on the lock file, as a second daemon process would hold it.
    fn other_handle(paths: &Paths) -> std::fs::File {
        let path = lock_path(paths);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap()
    }

    /// Two daemons starting after a stale socket: whoever holds the lock wins, and the other
    /// gets `AddrInUse` before it touches the socket.
    #[tokio::test]
    async fn bind_is_exclusive_through_the_lock_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path().to_path_buf());

        // Another daemon has taken the lock but not yet created its socket.
        let other = other_handle(&paths);
        fs4::FileExt::try_lock(&other).unwrap();
        let err = bind(&paths).await.expect_err("lock is held elsewhere");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse, "{err}");
        #[cfg(unix)]
        assert!(!std::path::Path::new(&socket_display(&paths)).exists());
        drop(other);

        let listener = bind(&paths).await.unwrap();
        // While the listener lives, nobody else can take the lock...
        let probe = other_handle(&paths);
        assert!(matches!(
            fs4::FileExt::try_lock(&probe),
            Err(fs4::TryLockError::WouldBlock)
        ));
        // ...and a second bind in this process, on its own handle, is refused.
        let err = bind(&paths).await.expect_err("already bound");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse, "{err}");
        drop(listener);
    }
}
