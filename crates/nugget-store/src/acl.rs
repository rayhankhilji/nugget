//! Owner-only access control for the data directory, the key file and the database files.
//!
//! On Unix this is handled with modes (`0700`/`0600`) where those files are created, so these
//! functions are no-ops there. On Windows they replace the object's DACL with a **protected**
//! one (no ACEs inherited from the parent) that grants full control to the current user and
//! `SYSTEM` only; a directory's ACEs are inherited by everything created inside it.
//!
//! Best effort: a failure is logged, never returned. The store content is encrypted either way,
//! and refusing to start on an unusual volume would be worse. Volumes without persistent ACLs
//! (FAT, exFAT, some network shares) cannot be protected at all; that is logged as a warning.

use std::path::Path;

/// Restrict `path` (a directory) to the current user and `SYSTEM`, inherited by its children.
pub(crate) fn restrict_dir(path: &Path) {
    #[cfg(windows)]
    win::restrict(path, true);
    #[cfg(not(windows))]
    let _ = path;
}

/// Restrict `path` (a file) to the current user and `SYSTEM`.
pub(crate) fn restrict_file(path: &Path) {
    #[cfg(windows)]
    win::restrict(path, false);
    #[cfg(not(windows))]
    let _ = path;
}

/// The Win32 calls: current user SID, `SYSTEM` SID, a two-ACE DACL, `SetNamedSecurityInfoW`.
#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, MAX_PATH};
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_FLAGS, ACL, ACL_REVISION, AddAccessAllowedAceEx,
        CONTAINER_INHERIT_ACE, CreateWellKnownSid, DACL_SECURITY_INFORMATION, GetLengthSid,
        GetTokenInformation, InitializeAcl, OBJECT_INHERIT_ACE,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER,
        TokenUser, WinLocalSystemSid,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALL_ACCESS, GetVolumeInformationW, GetVolumePathNameW,
    };
    use windows_sys::Win32::System::SystemServices::FILE_PERSISTENT_ACLS;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    pub(super) fn restrict(path: &Path, dir: bool) {
        let wide = to_wide(path.as_os_str());
        match persistent_acls(&wide) {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(
                    path = %path.display(),
                    "the volume holding this path does not support access control lists; other \
                     local users may be able to read it (the store itself stays encrypted)"
                );
                return;
            }
            // Cannot tell (e.g. an unusual network path): try anyway, a failure is logged below.
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "cannot query volume flags");
            }
        }
        if let Err(e) = set_protected_dacl(&wide, dir) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "cannot restrict access to the current user"
            );
        }
    }

    /// `s` as a NUL-terminated UTF-16 string.
    fn to_wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Whether the volume holding `path` (NUL-terminated) keeps ACLs (`FILE_PERSISTENT_ACLS`).
    fn persistent_acls(path: &[u16]) -> io::Result<bool> {
        // Long enough for any volume mount point of a path we can open; a longer one fails with
        // an error, which the caller treats as "unknown".
        let mut root = vec![0u16; MAX_PATH as usize + 1];
        // SAFETY: `path` is NUL-terminated; `root` is writable for `root.len()` UTF-16 units.
        let ok = unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut flags = 0u32;
        // SAFETY: `root` was NUL-terminated by GetVolumePathNameW; only the flags are requested,
        // every other out pointer is null with a zero length, as the API allows.
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut flags,
                ptr::null_mut(),
                0,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(flags & FILE_PERSISTENT_ACLS != 0)
    }

    /// Replace the DACL of `path` (NUL-terminated) with a protected one granting full control to
    /// the current user and `SYSTEM`. For a directory the ACEs are inherited by files and
    /// subdirectories.
    fn set_protected_dacl(path: &[u16], dir: bool) -> io::Result<()> {
        let user = current_token_user()?;
        let system = system_sid()?;
        let sids = [user.sid(), system.sid()];

        // ACL header plus one ACCESS_ALLOWED_ACE per SID, whose trailing `SidStart` field is
        // replaced by the SID itself (the documented size formula).
        let ace_base = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>();
        let mut len = size_of::<ACL>();
        for &sid in &sids {
            // SAFETY: `sid` is a valid SID kept alive by `user` / `system`.
            len += ace_base + unsafe { GetLengthSid(sid) } as usize;
        }
        // `u64` storage keeps the ACL DWORD-aligned, as InitializeAcl requires.
        let mut buf = vec![0u64; len.div_ceil(8)];
        let acl = buf.as_mut_ptr().cast::<ACL>();
        let len = u32::try_from(len).map_err(|_| io::Error::other("ACL too large"))?;
        // SAFETY: `acl` points to `buf`, aligned and writable for at least `len` bytes.
        if unsafe { InitializeAcl(acl, len, ACL_REVISION) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let inherit: ACE_FLAGS = if dir {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            0
        };
        for &sid in &sids {
            // SAFETY: `acl` was initialized above with room for both ACEs; `sid` is valid.
            let ok =
                unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, inherit, FILE_ALL_ACCESS, sid) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: `path` is NUL-terminated; `acl` is a valid ACL that outlives the call; owner,
        // group and SACL are not being set, so null is allowed for them.
        let err = unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                acl,
                ptr::null(),
            )
        };
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        Ok(())
    }

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

    /// The user of the current process token.
    fn current_token_user() -> io::Result<TokenUserBuf> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: GetCurrentProcess returns an always-valid pseudo handle (never closed);
        // `token` is a valid out pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
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

    /// The well-known `SYSTEM` (LocalSystem) SID, in an aligned buffer of the maximum SID size.
    struct SidBuf(Vec<u64>);

    impl SidBuf {
        fn sid(&self) -> PSID {
            self.0.as_ptr().cast_mut().cast()
        }
    }

    fn system_sid() -> io::Result<SidBuf> {
        let mut buf = SidBuf(vec![0u64; (SECURITY_MAX_SID_SIZE as usize).div_ceil(8)]);
        let mut len = SECURITY_MAX_SID_SIZE;
        // SAFETY: `buf` is writable for `len` (SECURITY_MAX_SID_SIZE) bytes; a machine-local
        // well-known SID needs no domain SID, so null is allowed.
        let ok = unsafe {
            CreateWellKnownSid(
                WinLocalSystemSid,
                ptr::null_mut(),
                buf.0.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buf)
    }
}
