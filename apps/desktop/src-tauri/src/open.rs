//! Opening search results and OAuth pages with the OS default handler.
//!
//! URLs reaching [`classify`] come from the webview, which renders data from indexed content
//! (mail, notes, files) and from the daemon, so they are treated as hostile: only `https:`,
//! `obsidian:` and local `file:` URLs are accepted, and a file that could run code when opened is
//! shown in its folder instead of being launched.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::Url;

/// Longer URLs are rejected outright (an OAuth consent URL is well under 2 KiB).
pub const MAX_URL_LEN: usize = 8 * 1024;

/// What a validated URL opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An `https:` page, opened in the default browser.
    Web(Url),
    /// An `obsidian:` link, opened by the Obsidian app if it is installed.
    Obsidian(Url),
    /// A local file or folder from a `file:` URL.
    File(PathBuf),
}

/// What [`open`] did, so the UI can explain a reveal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Opened {
    /// Handed to the OS default handler.
    Opened,
    /// The file could run code, so its folder was shown with the file selected.
    Revealed,
}

/// Validate `raw` and decide how to open it. Errors are user-facing messages.
pub fn classify(raw: &str) -> Result<Target, String> {
    if raw.is_empty() {
        return Err("There is no link to open.".into());
    }
    if raw.len() > MAX_URL_LEN {
        return Err("That link is too long to open.".into());
    }
    // `Url::parse` silently strips some of these; reject them instead of guessing intent.
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("That link contains spaces or control characters.".into());
    }
    let url = Url::parse(raw).map_err(|_| "That isn't a valid link.".to_string())?;
    match url.scheme() {
        "https" => {
            if url.host_str().is_none_or(str::is_empty) {
                return Err("That web link has no host.".into());
            }
            Ok(Target::Web(url))
        }
        "obsidian" => Ok(Target::Obsidian(url)),
        "file" => file_target(&url).map(Target::File),
        other => Err(format!(
            "Nugget only opens https, obsidian and file links, not “{other}:”."
        )),
    }
}

fn file_target(url: &Url) -> Result<PathBuf, String> {
    // A host means a network share (`file://server/share`): opening it can leak credentials to
    // that server (SMB/NTLM), so only local files are opened.
    if url
        .host_str()
        .is_some_and(|h| !h.is_empty() && h != "localhost")
    {
        return Err("Nugget only opens files on this device.".into());
    }
    let path = url
        .to_file_path()
        .map_err(|()| "That file link doesn't name a local file.".to_string())?;
    if !path.is_absolute() || is_unc(&path) {
        return Err("Nugget only opens files on this device.".into());
    }
    Ok(path)
}

/// `\\server\share`, `//server/share` and Windows device paths (`\\?\`, `\\.\`).
fn is_unc(path: &Path) -> bool {
    let s = path.as_os_str().to_string_lossy();
    s.starts_with(r"\\") || s.starts_with("//")
}

/// Extensions that run code (or install software) when opened with the default handler on at
/// least one desktop OS. Matched case-insensitively.
const RUNNABLE_EXTENSIONS: &[&str] = &[
    // Windows
    "exe",
    "com",
    "bat",
    "cmd",
    "msi",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "msp",
    "mst",
    "scr",
    "pif",
    "cpl",
    "lnk",
    "url",
    "reg",
    "hta",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "ws",
    "ps1",
    "psm1",
    "psd1",
    "ps1xml",
    "msc",
    "gadget",
    "inf",
    "settingcontent-ms",
    "library-ms",
    "search-ms",
    "application",
    "appref-ms",
    "chm",
    "diagcab",
    "xll",
    "jar",
    // macOS
    "app",
    "command",
    "tool",
    "terminal",
    "workflow",
    "action",
    "scpt",
    "scptd",
    "applescript",
    "pkg",
    "mpkg",
    "dmg",
    "prefpane",
    "webloc",
    "inetloc",
    "fileloc",
    // Linux and shells
    "sh",
    "bash",
    "zsh",
    "csh",
    "ksh",
    "fish",
    "run",
    "bin",
    "appimage",
    "desktop",
    "deb",
    "rpm",
    "flatpakref",
    "snap",
    // Interpreted scripts some systems associate with their interpreter
    "py",
    "pyw",
    "pyz",
    "pl",
    "rb",
    "php",
    "lua",
    "tcl",
];

/// True when opening `path` with the default handler could run code, so it should be revealed
/// in its folder instead.
pub fn should_reveal(path: &Path) -> bool {
    let runnable_ext = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        RUNNABLE_EXTENSIONS
            .iter()
            .any(|r| r.eq_ignore_ascii_case(e))
    });
    runnable_ext || is_executable_file(path)
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(_path: &Path) -> bool {
    false
}

/// Open a validated target with the OS default handler (never a program the caller picks).
pub fn open(target: Target) -> Result<Opened, String> {
    match target {
        Target::Web(url) | Target::Obsidian(url) => {
            tauri_plugin_opener::open_url(url.as_str(), None::<&str>)
                .map(|()| Opened::Opened)
                .map_err(|e| format!("Couldn't open the link: {e}"))
        }
        Target::File(path) => {
            if !path.exists() {
                return Err(
                    "That file no longer exists. It may have been moved or deleted.".into(),
                );
            }
            if should_reveal(&path) {
                tauri_plugin_opener::reveal_item_in_dir(&path)
                    .map(|()| Opened::Revealed)
                    .map_err(|e| format!("Couldn't show the file: {e}"))
            } else {
                tauri_plugin_opener::open_path(&path, None::<&str>)
                    .map(|()| Opened::Opened)
                    .map_err(|e| format!("Couldn't open the file: {e}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected(url: &str) -> bool {
        classify(url).is_err()
    }

    #[test]
    fn accepts_https_obsidian_and_local_files() {
        assert!(matches!(
            classify("https://mail.google.com/mail/u/0/#inbox/abc"),
            Ok(Target::Web(_))
        ));
        assert!(matches!(
            classify(
                "https://accounts.google.com/o/oauth2/v2/auth?client_id=x&redirect_uri=http%3A%2F%2F127.0.0.1%3A5000"
            ),
            Ok(Target::Web(_))
        ));
        assert!(matches!(
            classify("obsidian://open?vault=Notes&file=Daily%2F2026-10-28"),
            Ok(Target::Obsidian(_))
        ));
        // Scheme matching is case-insensitive (the URL parser lowercases it).
        assert!(matches!(
            classify("HTTPS://example.com/"),
            Ok(Target::Web(_))
        ));

        #[cfg(unix)]
        {
            assert_eq!(
                classify("file:///home/me/Notes/a%20b.md"),
                Ok(Target::File(PathBuf::from("/home/me/Notes/a b.md")))
            );
            assert_eq!(
                classify("file://localhost/tmp/x.txt"),
                Ok(Target::File(PathBuf::from("/tmp/x.txt")))
            );
        }
        #[cfg(windows)]
        assert_eq!(
            classify("file:///C:/Users/me/a.md"),
            Ok(Target::File(PathBuf::from(r"C:\Users\me\a.md")))
        );
    }

    #[test]
    fn rejects_script_and_other_schemes() {
        for url in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "http://example.com/",
            "ftp://example.com/file",
            "mailto:me@example.com",
            "smb://server/share",
            "ms-msdt:/id PCWDiagnostic",
            "search-ms:query=x",
            "tauri://localhost/",
            "ipc://localhost/open_url",
            "blob:https://example.com/uuid",
            "about:blank",
            "chrome://settings",
            "x-apple.systempreferences:com.apple.preference",
        ] {
            assert!(rejected(url), "{url} must be rejected");
        }
    }

    #[test]
    fn rejects_malformed_input() {
        for url in [
            "",
            "not a url",
            "/etc/passwd",
            "C:\\Windows\\System32\\calc.exe",
            "https://",
            " https://example.com/",
            "https://example.com/\n",
            "https://exa\u{0}mple.com/",
            "java\tscript:alert(1)",
            "https://example.com/a b",
        ] {
            assert!(rejected(url), "{url:?} must be rejected");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN));
        assert!(rejected(&long));
    }

    #[test]
    fn rejects_network_file_urls() {
        assert!(rejected("file://server/share/doc.txt"));
        assert!(rejected("file://192.168.1.10/share/doc.txt"));
        // The URL parser collapses the extra slashes, so this names a local path, not a share.
        #[cfg(unix)]
        assert_eq!(
            classify("file:////server/share/doc.txt"),
            Ok(Target::File(PathBuf::from("/server/share/doc.txt")))
        );
        #[cfg(windows)]
        assert!(rejected("file:////server/share/doc.txt"));
    }

    #[test]
    fn runnable_files_are_revealed_not_opened() {
        for p in [
            "/tmp/setup.exe",
            "/tmp/SETUP.EXE",
            "/tmp/run.sh",
            "/tmp/Calculator.app",
            "/tmp/script.ps1",
            "/tmp/shortcut.lnk",
            "/tmp/tool.py",
            "/tmp/x.desktop",
            "/tmp/x.AppImage",
        ] {
            assert!(should_reveal(Path::new(p)), "{p} should be revealed");
        }
        for p in [
            "/tmp/notes.md",
            "/tmp/report.pdf",
            "/tmp/photo.JPG",
            "/tmp/no-such-dir/",
        ] {
            assert!(!should_reveal(Path::new(p)), "{p} should open");
        }
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_means_reveal() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("nugget-open-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("notes");
        let exe = dir.join("tool");
        std::fs::write(&plain, b"hi").unwrap();
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!should_reveal(&plain));
        assert!(should_reveal(&exe));
        // Directories have execute bits but open in the file manager.
        assert!(!should_reveal(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opened_serializes_for_the_frontend() {
        assert_eq!(serde_json::to_value(Opened::Revealed).unwrap(), "revealed");
        assert_eq!(serde_json::to_value(Opened::Opened).unwrap(), "opened");
    }
}
