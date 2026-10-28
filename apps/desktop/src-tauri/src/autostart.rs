//! "Open Nugget at login": starts the desktop app itself, hidden in the tray (`--minimized`).
//! Separate from `login.rs`, which registers the `nuggetd` background service.
//!
//! The OS entry is the source of truth, so turning it off in the system's own settings sticks:
//! - macOS: `~/Library/LaunchAgents/dev.nugget.desktop.autostart.plist` (RunAtLoad).
//! - Linux: `$XDG_CONFIG_HOME/autostart/dev.nugget.desktop.desktop` (XDG autostart). An AppImage
//!   is started through its `$APPIMAGE` file, not the temporary mount.
//! - Windows: the `NuggetDesktop` value under `HKCU\...\CurrentVersion\Run`, written with `reg.exe`.
//!
//! `settings.autostart` mirrors the entry. At launch an enabled entry is rewritten so it follows
//! the app when it moves or updates, and a removed one turns the setting off.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::daemon::CmdError;
use crate::settings::Settings;
use crate::updater::Updates;

/// Passed by the login entry: start hidden in the tray instead of opening the window.
pub const HIDDEN_ARG: &str = "--minimized";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AutostartStatus {
    /// This platform/build can register a login entry.
    pub supported: bool,
    pub enabled: bool,
}

/// True when this launch came from the login entry.
pub fn launched_hidden() -> bool {
    std::env::args().skip(1).any(|a| a == HIDDEN_ARG)
}

/// The program the entry should start.
fn target_exe() -> Result<PathBuf, String> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Ok(PathBuf::from(appimage));
    }
    let exe = std::env::current_exe().map_err(|e| format!("can't locate Nugget: {e}"))?;
    // A copy run from the disk image or a translocated path won't be there after a reboot.
    if let Some(why) = crate::login::transient_location(&exe, false) {
        return Err(why.into());
    }
    Ok(exe)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// Escape one argument for a desktop entry `Exec` key: quoted per the spec's argument rules,
/// then backslashes doubled again for the string-level escaping, and `%` doubled.
pub fn desktop_exec_arg(arg: &str) -> String {
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted.replace('\\', "\\\\")
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Nugget\nComment=Open Nugget in the tray at login\nExec={} {HIDDEN_ARG}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        desktop_exec_arg(&exe.to_string_lossy())
    )
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn launch_agent(label: &str, exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>{HIDDEN_ARG}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        xml_escape(label),
        xml_escape(&exe.to_string_lossy())
    )
}

/// The entry for platforms that use a file (macOS, Linux).
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod file_entry {
    use super::*;

    #[cfg(target_os = "macos")]
    const LABEL: &str = "dev.nugget.desktop.autostart";

    fn path() -> Result<PathBuf, String> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from);
        #[cfg(target_os = "macos")]
        let p = home.map(|h| {
            h.join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist"))
        });
        #[cfg(target_os = "linux")]
        let p = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.map(|h| h.join(".config")))
            .map(|c| c.join("autostart/dev.nugget.desktop.desktop"));
        p.ok_or_else(|| "no home directory".into())
    }

    pub fn is_enabled() -> bool {
        path().is_ok_and(|p| p.is_file())
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        let path = path()?;
        if !enabled {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("couldn't remove {}: {e}", path.display()))
                }
                _ => Ok(()),
            };
        }
        let exe = target_exe()?;
        #[cfg(target_os = "macos")]
        let body = launch_agent(LABEL, &exe);
        #[cfg(target_os = "linux")]
        let body = desktop_entry(&exe);
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, body)
        };
        write().map_err(|e| format!("couldn't write {}: {e}", path.display()))
    }
}

#[cfg(windows)]
mod reg_entry {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    use super::*;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "NuggetDesktop";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("reg.exe")
            .args(args)
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    }

    pub fn is_enabled() -> bool {
        reg(&["query", KEY, "/v", VALUE]).is_ok_and(|o| o.status.success())
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        let out = if enabled {
            let exe = target_exe()?;
            let data = format!("\"{}\" {HIDDEN_ARG}", exe.display());
            reg(&[
                "add",
                KEY,
                "/v",
                VALUE,
                "/t",
                "REG_SZ",
                "/d",
                data.as_str(),
                "/f",
            ])
        } else {
            if !is_enabled() {
                return Ok(());
            }
            reg(&["delete", KEY, "/v", VALUE, "/f"])
        }
        .map_err(|e| format!("couldn't run reg.exe: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
use file_entry as entry;
#[cfg(windows)]
use reg_entry as entry;

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn status() -> AutostartStatus {
    AutostartStatus {
        supported: true,
        enabled: entry::is_enabled(),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn status() -> AutostartStatus {
    AutostartStatus {
        supported: false,
        enabled: false,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn apply(enabled: bool) -> Result<(), String> {
    entry::set(enabled)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn apply(_enabled: bool) -> Result<(), String> {
    Err("not supported on this platform".into())
}

fn remember(app: &AppHandle, enabled: bool) -> Result<(), CmdError> {
    let updates = app.state::<Updates>();
    let current = updates.settings();
    if current.autostart != enabled {
        updates.set_settings(
            app,
            Settings {
                autostart: enabled,
                ..current
            },
        )?;
    }
    Ok(())
}

/// At launch: refresh an enabled entry (the app may have moved or updated), and follow the OS
/// when the user removed it there.
pub fn reconcile(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let wanted = app.state::<Updates>().settings().autostart;
        let present = status().enabled;
        let result = match (wanted, present) {
            (true, true) => apply(true).map_err(CmdError::Other),
            (true, false) | (false, true) => remember(&app, present),
            (false, false) => Ok(()),
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, "couldn't refresh the login entry");
        }
    });
}

#[tauri::command]
pub async fn autostart_get() -> Result<AutostartStatus, CmdError> {
    tauri::async_runtime::spawn_blocking(status)
        .await
        .map_err(|e| CmdError::Other(e.to_string()))
}

/// Add or remove the login entry, then record the choice.
#[tauri::command]
pub async fn autostart_set(app: AppHandle, enabled: bool) -> Result<AutostartStatus, CmdError> {
    tauri::async_runtime::spawn_blocking(move || {
        apply(enabled).map_err(CmdError::Other)?;
        tracing::info!(enabled, "open at login");
        remember(&app, enabled)?;
        Ok(status())
    })
    .await
    .map_err(|e| CmdError::Other(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_exec_quotes_and_escapes() {
        assert_eq!(desktop_exec_arg("/usr/bin/nugget"), r#""/usr/bin/nugget""#);
        assert_eq!(
            desktop_exec_arg("/home/a b/Nugget 100%.AppImage"),
            r#""/home/a b/Nugget 100%%.AppImage""#
        );
        // `$` and `"` get one backslash for the argument, which is then doubled for the string.
        assert_eq!(desktop_exec_arg(r#"/x/$HOME"q"#), r#""/x/\\$HOME\\"q""#);
        assert_eq!(desktop_exec_arg(r"/x\y"), r#""/x\\\\y""#);
        let entry = desktop_entry(Path::new("/opt/Nugget/nugget"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nExec=\"/opt/Nugget/nugget\" --minimized\n"));
        assert!(!entry.contains("Hidden=true"));
    }

    #[test]
    fn launch_agent_escapes_paths() {
        let plist = launch_agent(
            "dev.nugget.desktop.autostart",
            Path::new("/Apps/A&B <x>.app/n"),
        );
        assert!(plist.contains("<string>/Apps/A&amp;B &lt;x&gt;.app/n</string>"));
        assert!(plist.contains("<string>--minimized</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
    }
}
