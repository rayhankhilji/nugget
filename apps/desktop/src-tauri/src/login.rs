//! "Start Nugget at login": runs the bundled `nuggetd install-service` / `uninstall-service`
//! (launchd, systemd --user, Task Scheduler), only when the user says yes in the app.
//!
//! Installers don't do this themselves: deb/rpm scripts run as root for the whole machine, and
//! the macOS bundle has no install step, while the service is per-user. The Windows NSIS hooks
//! only stop the daemon before files are replaced and remove the task on uninstall.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::daemon::{CmdError, find_nuggetd};

/// Why a service can't point at this copy of `nuggetd`: its path won't exist after this run.
pub fn transient_location(exe: &Path, appimage: bool) -> Option<&'static str> {
    if appimage {
        return Some(
            "The AppImage runs from a temporary mount, so a login service can't point at it. Install the .deb or .rpm package, or copy nuggetd somewhere permanent and run `nuggetd install-service`.",
        );
    }
    let s = exe.to_string_lossy();
    if cfg!(target_os = "macos") && (s.starts_with("/Volumes/") || s.contains("/AppTranslocation/"))
    {
        return Some("Move Nugget to your Applications folder first, then try again.");
    }
    None
}

/// Run `nuggetd <install-service|uninstall-service>` and return what it printed.
pub fn run(install: bool) -> Result<String, CmdError> {
    let exe = find_nuggetd().ok_or_else(|| {
        CmdError::Spawn("Couldn't find nuggetd next to the app or on PATH.".into())
    })?;
    if install && let Some(why) = transient_location(&exe, std::env::var_os("APPIMAGE").is_some()) {
        return Err(CmdError::Other(why.into()));
    }
    let verb = if install {
        "install-service"
    } else {
        "uninstall-service"
    };
    let mut cmd = Command::new(&exe);
    cmd.arg(verb).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .map_err(|e| CmdError::Spawn(format!("Couldn't run {}: {e}", exe.display())))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        Err(CmdError::Other(if stderr.is_empty() {
            format!("nuggetd {verb} failed ({})", out.status)
        } else {
            stderr
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_transient_locations() {
        let installed = Path::new("/usr/bin/nuggetd");
        assert!(transient_location(installed, true).is_some());
        assert_eq!(transient_location(installed, false), None);
        let dmg = Path::new("/Volumes/Nugget/Nugget.app/Contents/MacOS/nuggetd");
        assert_eq!(
            transient_location(dmg, false).is_some(),
            cfg!(target_os = "macos")
        );
    }
}
