//! Native-messaging host registration per browser and OS (user level, no admin rights).
//!
//! | OS      | Chromium family                                   | Firefox                                     |
//! |---------|---------------------------------------------------|---------------------------------------------|
//! | macOS   | `~/Library/Application Support/<vendor>/NativeMessagingHosts/` | `~/Library/Application Support/Mozilla/NativeMessagingHosts/` |
//! | Linux   | `~/.config/<vendor>/NativeMessagingHosts/`        | `~/.mozilla/native-messaging-hosts/`        |
//! | Windows | manifest under `%LOCALAPPDATA%\Nugget\NativeMessagingHosts\<browser>\`, plus `HKCU\Software\<vendor>\NativeMessagingHosts\dev.nugget.browser` pointing at it |
//!
//! [`plan`] is pure (golden-tested for every browser and OS); [`apply`] performs a plan, or with
//! `dry_run` only reports it. Registry steps run `reg.exe` and exist only on Windows.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

/// Native-messaging host name (the extension calls `connectNative` with it).
pub const HOST_NAME: &str = "dev.nugget.browser";
/// Extension id of the unpacked development build (pinned by the public `key` in
/// `extensions/browser/static/manifest.base.json`). Store builds pass their own id.
pub const CHROMIUM_DEV_EXTENSION_ID: &str = "gljflghdpajgdddaibpdgjibnkeggchn";
/// Firefox add-on id (`browser_specific_settings.gecko.id` in the Firefox build).
pub const FIREFOX_EXTENSION_ID: &str = "browser@nugget.dev";
const DESCRIPTION: &str = "Nugget browser bridge";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Browser {
    Chrome,
    Chromium,
    Edge,
    Firefox,
}

impl Browser {
    pub const ALL: [Browser; 4] = [Self::Chrome, Self::Chromium, Self::Edge, Self::Firefox];

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "chrome" | "google-chrome" => Some(Self::Chrome),
            "chromium" => Some(Self::Chromium),
            "edge" | "msedge" | "microsoft-edge" => Some(Self::Edge),
            "firefox" => Some(Self::Firefox),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Chromium => "chromium",
            Self::Edge => "edge",
            Self::Firefox => "firefox",
        }
    }

    pub fn is_chromium_family(self) -> bool {
        !matches!(self, Self::Firefox)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }

    fn sep(self) -> char {
        if self == Self::Windows { '\\' } else { '/' }
    }
}

/// Everything [`plan`] needs; paths are strings in the target OS's syntax so plans for any OS
/// can be produced (and golden-tested) on any OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallConfig {
    pub os: Os,
    /// Home directory (macOS, Linux).
    pub home: String,
    /// `%LOCALAPPDATA%` (Windows).
    pub local_app_data: Option<String>,
    /// Absolute path of the `nugget-browser-host` executable.
    pub host_path: String,
    /// Chromium-family extension ids allowed to connect.
    pub chromium_extension_ids: Vec<String>,
    /// Firefox add-on ids allowed to connect.
    pub firefox_extension_ids: Vec<String>,
}

impl InstallConfig {
    /// Defaults for this machine and the current executable.
    pub fn for_current_user(host_path: PathBuf) -> io::Result<Self> {
        let os = Os::current();
        let home = std::env::var_os(if os == Os::Windows {
            "USERPROFILE"
        } else {
            "HOME"
        })
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory not set"))?;
        let local_app_data =
            std::env::var_os("LOCALAPPDATA").map(|p| p.to_string_lossy().into_owned());
        Ok(Self {
            os,
            home: home.to_string_lossy().into_owned(),
            local_app_data,
            host_path: host_path.to_string_lossy().into_owned(),
            chromium_extension_ids: vec![CHROMIUM_DEV_EXTENSION_ID.to_owned()],
            firefox_extension_ids: vec![FIREFOX_EXTENSION_ID.to_owned()],
        })
    }

    fn join(&self, base: &str, parts: &[&str]) -> String {
        let sep = self.os.sep();
        let mut out = base.trim_end_matches(sep).to_owned();
        for p in parts {
            out.push(sep);
            out.push_str(p);
        }
        out
    }
}

/// The JSON manifest a browser reads to find and authorize the host.
pub fn manifest(browser: Browser, cfg: &InstallConfig) -> Value {
    if browser.is_chromium_family() {
        let origins: Vec<String> = cfg
            .chromium_extension_ids
            .iter()
            .map(|id| format!("chrome-extension://{id}/"))
            .collect();
        json!({
            "name": HOST_NAME,
            "description": DESCRIPTION,
            "path": cfg.host_path,
            "type": "stdio",
            "allowed_origins": origins,
        })
    } else {
        json!({
            "name": HOST_NAME,
            "description": DESCRIPTION,
            "path": cfg.host_path,
            "type": "stdio",
            "allowed_extensions": cfg.firefox_extension_ids,
        })
    }
}

/// Where the manifest goes.
pub fn manifest_path(browser: Browser, cfg: &InstallConfig) -> Result<String, InstallError> {
    let file = format!("{HOST_NAME}.json");
    Ok(match cfg.os {
        Os::MacOs => {
            let vendor = match browser {
                Browser::Chrome => "Google/Chrome",
                Browser::Chromium => "Chromium",
                Browser::Edge => "Microsoft Edge",
                Browser::Firefox => "Mozilla",
            };
            let mut parts: Vec<&str> = vec!["Library", "Application Support"];
            parts.extend(vendor.split('/'));
            parts.push("NativeMessagingHosts");
            parts.push(&file);
            cfg.join(&cfg.home, &parts)
        }
        Os::Linux => match browser {
            Browser::Chrome => cfg.join(
                &cfg.home,
                &[".config", "google-chrome", "NativeMessagingHosts", &file],
            ),
            Browser::Chromium => cfg.join(
                &cfg.home,
                &[".config", "chromium", "NativeMessagingHosts", &file],
            ),
            Browser::Edge => cfg.join(
                &cfg.home,
                &[".config", "microsoft-edge", "NativeMessagingHosts", &file],
            ),
            Browser::Firefox => cfg.join(&cfg.home, &[".mozilla", "native-messaging-hosts", &file]),
        },
        Os::Windows => {
            let base = cfg
                .local_app_data
                .as_deref()
                .ok_or(InstallError::MissingLocalAppData)?;
            cfg.join(
                base,
                &["Nugget", "NativeMessagingHosts", browser.name(), &file],
            )
        }
    })
}

/// The per-user registry key (Windows only).
pub fn registry_key(browser: Browser) -> String {
    let vendor = match browser {
        Browser::Chrome => r"Google\Chrome",
        Browser::Chromium => "Chromium",
        Browser::Edge => r"Microsoft\Edge",
        Browser::Firefox => "Mozilla",
    };
    format!(r"HKCU\Software\{vendor}\NativeMessagingHosts\{HOST_NAME}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    WriteFile {
        path: String,
        contents: String,
    },
    RemoveFile {
        path: String,
    },
    /// Set the key's default value.
    SetRegistryDefault {
        key: String,
        value: String,
    },
    DeleteRegistryKey {
        key: String,
    },
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WriteFile { path, .. } => write!(f, "write {path}"),
            Self::RemoveFile { path } => write!(f, "remove {path}"),
            Self::SetRegistryDefault { key, value } => write!(f, "set {key} = {value}"),
            Self::DeleteRegistryKey { key } => write!(f, "delete registry key {key}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("LOCALAPPDATA is not set")]
    MissingLocalAppData,
    #[error("the host path must be absolute: {0}")]
    RelativeHostPath(String),
    #[error("not a valid extension id: {0:?}")]
    BadExtensionId(String),
    #[error("registry changes are only possible on Windows")]
    RegistryUnsupported,
    #[error("reg.exe failed for {key}: {detail}")]
    Registry { key: String, detail: String },
    #[error("{path}: {source}")]
    Io { path: String, source: io::Error },
}

fn is_absolute(os: Os, p: &str) -> bool {
    match os {
        Os::Windows => {
            let b = p.as_bytes();
            (b.len() >= 3
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && (b[2] == b'\\' || b[2] == b'/'))
                || p.starts_with(r"\\")
        }
        _ => p.starts_with('/'),
    }
}

/// A Chromium extension id: 32 characters `a`-`p` (no wildcards: `allowed_origins` must name
/// exactly the extensions that may start the host).
fn valid_chromium_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// A Firefox add-on id: `name@domain` or `{GUID}`, with no wildcard or path characters.
fn valid_firefox_id(id: &str) -> bool {
    let ok_chars = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
    };
    let guid = id.len() == 38
        && id.starts_with('{')
        && id.ends_with('}')
        && id[1..37]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-');
    let email = id.len() <= 255
        && id
            .split_once('@')
            .is_some_and(|(a, b)| ok_chars(a) && ok_chars(b) && !b.contains('@'));
    guid || email
}

/// Refuse wildcard, empty or malformed extension ids before they reach a manifest.
fn check_ids(browsers: &[Browser], cfg: &InstallConfig) -> Result<(), InstallError> {
    if browsers.iter().any(|b| b.is_chromium_family()) {
        if cfg.chromium_extension_ids.is_empty() {
            return Err(InstallError::BadExtensionId(String::new()));
        }
        if let Some(bad) = cfg
            .chromium_extension_ids
            .iter()
            .find(|i| !valid_chromium_id(i))
        {
            return Err(InstallError::BadExtensionId(bad.clone()));
        }
    }
    if browsers.contains(&Browser::Firefox) {
        if cfg.firefox_extension_ids.is_empty() {
            return Err(InstallError::BadExtensionId(String::new()));
        }
        if let Some(bad) = cfg
            .firefox_extension_ids
            .iter()
            .find(|i| !valid_firefox_id(i))
        {
            return Err(InstallError::BadExtensionId(bad.clone()));
        }
    }
    Ok(())
}

/// Steps to register the host with `browsers`.
pub fn plan(browsers: &[Browser], cfg: &InstallConfig) -> Result<Vec<Step>, InstallError> {
    if !is_absolute(cfg.os, &cfg.host_path) {
        return Err(InstallError::RelativeHostPath(cfg.host_path.clone()));
    }
    check_ids(browsers, cfg)?;
    let mut steps = Vec::new();
    for &b in browsers {
        let path = manifest_path(b, cfg)?;
        let mut contents =
            serde_json::to_string_pretty(&manifest(b, cfg)).expect("manifest serializes");
        contents.push('\n');
        steps.push(Step::WriteFile {
            path: path.clone(),
            contents,
        });
        if cfg.os == Os::Windows {
            steps.push(Step::SetRegistryDefault {
                key: registry_key(b),
                value: path,
            });
        }
    }
    Ok(steps)
}

/// Steps to unregister the host from `browsers`.
pub fn uninstall_plan(
    browsers: &[Browser],
    cfg: &InstallConfig,
) -> Result<Vec<Step>, InstallError> {
    let mut steps = Vec::new();
    for &b in browsers {
        if cfg.os == Os::Windows {
            steps.push(Step::DeleteRegistryKey {
                key: registry_key(b),
            });
        }
        steps.push(Step::RemoveFile {
            path: manifest_path(b, cfg)?,
        });
    }
    Ok(steps)
}

/// Perform `steps` (or, with `dry_run`, only describe them). Returns one line per step.
/// Removing something that is already gone is not an error.
pub fn apply(steps: &[Step], dry_run: bool) -> Result<Vec<String>, InstallError> {
    let mut log = Vec::new();
    for step in steps {
        if dry_run {
            log.push(format!("would {step}"));
            continue;
        }
        match step {
            Step::WriteFile { path, contents } => {
                write_atomic(Path::new(path), contents.as_bytes()).map_err(|source| {
                    InstallError::Io {
                        path: path.clone(),
                        source,
                    }
                })?
            }
            Step::RemoveFile { path } => match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(InstallError::Io {
                        path: path.clone(),
                        source,
                    });
                }
            },
            Step::SetRegistryDefault { key, value } => {
                registry(
                    &["add", key, "/ve", "/t", "REG_SZ", "/d", value, "/f"],
                    key,
                    false,
                )?;
            }
            Step::DeleteRegistryKey { key } => registry(&["delete", key, "/f"], key, true)?,
        }
        log.push(step.to_string());
    }
    Ok(log)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(windows)]
fn registry(args: &[&str], key: &str, missing_ok: bool) -> Result<(), InstallError> {
    let out = std::process::Command::new("reg")
        .args(args)
        .output()
        .map_err(|source| InstallError::Io {
            path: "reg.exe".into(),
            source,
        })?;
    if out.status.success() || missing_ok {
        Ok(())
    } else {
        Err(InstallError::Registry {
            key: key.to_owned(),
            detail: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

#[cfg(not(windows))]
fn registry(_args: &[&str], _key: &str, _missing_ok: bool) -> Result<(), InstallError> {
    Err(InstallError::RegistryUnsupported)
}
