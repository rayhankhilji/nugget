//! The apps of the matrix: where each is installed per OS, its version, launching it on a
//! fixture page or a seed document with a fresh profile and no first-run UI, bringing it to the
//! front, and killing it (with its helper processes) afterwards.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nugget_a11y::ComputerUse;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Chrome,
    Edge,
    Firefox,
    Safari,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Native {
    TextEdit,
    Notes,
    Notepad,
    WordPad,
    Gedit,
    GnomeTextEditor,
    Writer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Browser(Browser),
    Native(Native),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSpec {
    pub id: &'static str,
    pub kind: Kind,
}

impl AppSpec {
    pub fn is_chromium(&self) -> bool {
        matches!(self.kind, Kind::Browser(Browser::Chrome | Browser::Edge))
    }

    /// Names to look for in `ComputerUse::apps()` (lower case, matched as substrings of the app
    /// name or id).
    pub fn app_names(&self) -> &'static [&'static str] {
        match self.kind {
            Kind::Browser(Browser::Chrome) => &["google chrome", "chrome"],
            Kind::Browser(Browser::Edge) => &["microsoft edge", "msedge", "edge"],
            Kind::Browser(Browser::Firefox) => &["firefox"],
            Kind::Browser(Browser::Safari) => &["safari"],
            Kind::Native(Native::TextEdit) => &["textedit"],
            Kind::Native(Native::Notes) => &["com.apple.notes", "notes"],
            Kind::Native(Native::Notepad) => &["notepad"],
            Kind::Native(Native::WordPad) => &["wordpad"],
            Kind::Native(Native::Gedit) => &["gedit"],
            Kind::Native(Native::GnomeTextEditor) => &["text-editor", "text editor"],
            Kind::Native(Native::Writer) => &["libreoffice", "soffice"],
        }
    }
}

/// Every app this OS can have, in run order.
pub fn all_apps() -> Vec<AppSpec> {
    use Browser::*;
    use Native::*;
    let b = |id, b| AppSpec {
        id,
        kind: Kind::Browser(b),
    };
    let n = |id, n| AppSpec {
        id,
        kind: Kind::Native(n),
    };
    if cfg!(target_os = "macos") {
        vec![
            b("chrome", Chrome),
            b("edge", Edge),
            b("firefox", Firefox),
            b("safari", Safari),
            n("textedit", TextEdit),
            n("notes", Notes),
        ]
    } else if cfg!(windows) {
        vec![
            b("chrome", Chrome),
            b("edge", Edge),
            b("firefox", Firefox),
            n("notepad", Notepad),
            n("wordpad", WordPad),
        ]
    } else {
        vec![
            b("chrome", Chrome),
            b("edge", Edge),
            b("firefox", Firefox),
            n("gedit", Gedit),
            n("gnome-text-editor", GnomeTextEditor),
            n("writer", Writer),
        ]
    }
}

// ---- processes ----

/// Run a command with a deadline; its stdout (lossy UTF-8) when it exited successfully.
pub fn run_out(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    run_out_env(program, args, &[], timeout)
}

/// [`run_out`] with extra environment variables.
pub fn run_out_env(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Duration,
) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    // The pipes are read on a thread; a grandchild that keeps them open must not hang us, so
    // the result is awaited with a deadline too.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut o = Vec::new();
        let mut e = Vec::new();
        if let Some(s) = out.as_mut() {
            let _ = s.read_to_end(&mut o);
        }
        if let Some(s) = err.as_mut() {
            let _ = s.read_to_end(&mut e);
        }
        let _ = tx.send((o, e));
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} {args:?} timed out after {timeout:?}"));
            }
        }
    };
    let (o, e) = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    if status.success() {
        Ok(String::from_utf8_lossy(&o).into_owned())
    } else {
        Err(format!(
            "{program} {args:?} failed ({status}): {}",
            String::from_utf8_lossy(&e).trim()
        ))
    }
}

/// A program on `PATH`, or an absolute candidate that exists.
pub fn which(cands: &[&str]) -> Option<PathBuf> {
    cands.iter().find_map(|c| {
        let p = Path::new(c);
        if p.is_absolute() {
            return p.exists().then(|| p.to_path_buf());
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).find_map(|d| {
            let f = d.join(c);
            if f.is_file() {
                return Some(f);
            }
            let exe = d.join(format!("{c}.exe"));
            (cfg!(windows) && exe.is_file()).then_some(exe)
        })
    })
}

fn env_path(var: &str, rest: &[&str]) -> Option<PathBuf> {
    let mut p = PathBuf::from(std::env::var_os(var)?);
    for r in rest {
        p.push(r);
    }
    Some(p)
}

/// The executable (or, on macOS, the `.app` bundle) of an app. `NUGGET_A11Y_APPS_EXE_<ID>`
/// (id upper-cased, `-` as `_`) overrides the search.
pub fn locate(spec: &AppSpec) -> Result<PathBuf, String> {
    let var = format!(
        "NUGGET_A11Y_APPS_EXE_{}",
        spec.id.to_uppercase().replace('-', "_")
    );
    if let Some(p) = std::env::var_os(&var) {
        let p = PathBuf::from(p);
        return if p.exists() {
            Ok(p)
        } else {
            Err(format!("{var}={} does not exist", p.display()))
        };
    }
    let found = candidates(spec).into_iter().find(|p| p.exists());
    found.ok_or_else(|| {
        format!(
            "not installed (looked in standard locations; set {var} to override): {}",
            candidates(spec)
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

fn candidates(spec: &AppSpec) -> Vec<PathBuf> {
    use Browser::*;
    use Native::*;
    if cfg!(target_os = "macos") {
        let app = |s: &str| PathBuf::from(s);
        return match spec.kind {
            Kind::Browser(Chrome) => vec![app("/Applications/Google Chrome.app")],
            Kind::Browser(Edge) => vec![app("/Applications/Microsoft Edge.app")],
            Kind::Browser(Firefox) => vec![app("/Applications/Firefox.app")],
            Kind::Browser(Safari) => vec![app("/Applications/Safari.app")],
            Kind::Native(TextEdit) => vec![
                app("/System/Applications/TextEdit.app"),
                app("/Applications/TextEdit.app"),
            ],
            Kind::Native(Notes) => vec![
                app("/System/Applications/Notes.app"),
                app("/Applications/Notes.app"),
            ],
            _ => vec![],
        };
    }
    if cfg!(windows) {
        let pf = |rest: &[&str]| {
            ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
                .iter()
                .filter_map(|v| env_path(v, rest))
                .collect::<Vec<_>>()
        };
        return match spec.kind {
            Kind::Browser(Chrome) => pf(&["Google", "Chrome", "Application", "chrome.exe"]),
            Kind::Browser(Edge) => pf(&["Microsoft", "Edge", "Application", "msedge.exe"]),
            Kind::Browser(Firefox) => pf(&["Mozilla Firefox", "firefox.exe"]),
            Kind::Native(Notepad) => env_path("SystemRoot", &["System32", "notepad.exe"])
                .into_iter()
                .collect(),
            // write.exe only launches wordpad.exe, which is gone from Windows Server 2025.
            Kind::Native(WordPad) => pf(&["Windows NT", "Accessories", "wordpad.exe"]),
            _ => vec![],
        };
    }
    let w = |c: &[&str]| which(c).into_iter().collect::<Vec<_>>();
    match spec.kind {
        Kind::Browser(Chrome) => w(&[
            "google-chrome-stable",
            "google-chrome",
            "/opt/google/chrome/chrome",
        ]),
        Kind::Browser(Edge) => w(&[
            "microsoft-edge-stable",
            "microsoft-edge",
            "/opt/microsoft/msedge/msedge",
        ]),
        Kind::Browser(Firefox) => w(&[
            "firefox",
            "/usr/lib/firefox/firefox",
            "/opt/firefox/firefox",
        ]),
        Kind::Native(Gedit) => w(&["gedit"]),
        Kind::Native(GnomeTextEditor) => w(&["gnome-text-editor"]),
        Kind::Native(Writer) => w(&[
            "soffice",
            "libreoffice",
            "/usr/lib/libreoffice/program/soffice",
        ]),
        _ => vec![],
    }
}

/// The app's version, best effort (bounded).
pub fn version(spec: &AppSpec, path: &Path) -> Option<String> {
    let t = Duration::from_secs(15);
    let first_line = |s: String| s.lines().next().map(|l| l.trim().to_string());
    let p = path.to_string_lossy().into_owned();
    if cfg!(target_os = "macos") {
        let plist = path.join("Contents").join("Info.plist");
        let plist = plist.to_string_lossy();
        return run_out(
            "/usr/bin/plutil",
            &["-extract", "CFBundleShortVersionString", "raw", &plist],
            t,
        )
        .ok()
        .and_then(first_line);
    }
    if cfg!(windows) {
        let script = if spec.kind == Kind::Native(Native::Notepad) {
            "$p = Get-AppxPackage *WindowsNotepad* | Select-Object -First 1; \
             if ($p) { $p.Version } else { (Get-Item $env:NUGGET_EXE).VersionInfo.ProductVersion }"
                .to_string()
        } else {
            "(Get-Item $env:NUGGET_EXE).VersionInfo.ProductVersion".to_string()
        };
        let out = run_out_env(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
            &[("NUGGET_EXE", &p)],
            t,
        )
        .ok()?;
        return first_line(out).filter(|s| !s.is_empty());
    }
    let out = if spec.kind == Kind::Native(Native::Writer) {
        // A private user installation, so the version query never talks to a running office.
        let dir = std::env::temp_dir().join(format!("nugget-lo-version-{}", std::process::id()));
        let env = format!("-env:UserInstallation={}", file_url(&dir));
        let r = run_out(&p, &[&env, "--version"], t);
        let _ = std::fs::remove_dir_all(&dir);
        r
    } else {
        run_out(&p, &["--version"], t)
    };
    out.ok().and_then(first_line).filter(|s| !s.is_empty())
}

/// The OS name and version, best effort.
pub fn os_version() -> String {
    let t = Duration::from_secs(10);
    if cfg!(target_os = "macos") {
        let v = run_out("sw_vers", &["-productVersion"], t).unwrap_or_default();
        return format!("macOS {}", v.trim());
    }
    if cfg!(windows) {
        return run_out("cmd", &["/c", "ver"], t)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "Windows".into());
    }
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into())
}

/// `file://` URL of a local directory (for LibreOffice's `UserInstallation`).
pub fn file_url(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.replace(' ', "%20");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

fn running_as_root() -> bool {
    cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/self/status").is_ok_and(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1).map(|u| u == "0"))
                .unwrap_or(false)
        })
}

/// Chrome / Edge command line: fresh profile, no first-run UI, a new window on `url`.
pub fn chromium_args(edge: bool, url: &str, profile: &Path, force_a11y: bool) -> Vec<String> {
    let mut features = vec![
        "Translate",
        "OptimizationHints",
        "MediaRouter",
        "DialMediaRouteProvider",
        "AutofillServerCommunication",
        "PrivacySandboxSettings4",
    ];
    if edge {
        features.extend([
            "msEdgeTranslate",
            "msUndersideButton",
            "msEdgeSidebarV2",
            "msImplicitSignin",
        ]);
    }
    let mut a = vec![
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        format!("--disable-features={}", features.join(",")),
        "--disable-sync".into(),
        "--disable-extensions".into(),
        "--disable-component-update".into(),
        "--disable-background-networking".into(),
        "--disable-backgrounding-occluded-windows".into(),
        "--disable-renderer-backgrounding".into(),
        "--disable-background-timer-throttling".into(),
        "--disable-session-crashed-bubble".into(),
        "--hide-crash-restore-bubble".into(),
        "--password-store=basic".into(),
        "--use-mock-keychain".into(),
        "--no-proxy-server".into(),
        "--window-position=40,40".into(),
        "--window-size=1100,850".into(),
    ];
    if edge {
        a.push("--edge-skip-compat-layer-relaunch".into());
    }
    if cfg!(target_os = "linux") {
        a.push("--ozone-platform=x11".into());
        a.push("--disable-gpu".into());
        if running_as_root() {
            a.push("--no-sandbox".into());
        }
    }
    if force_a11y {
        a.push("--force-renderer-accessibility".into());
    }
    a.push("--new-window".into());
    a.push(url.into());
    a
}

/// Firefox preferences for a fresh profile: no welcome, default-browser, telemetry, update or
/// session-restore UI.
pub const FIREFOX_USER_JS: &str = r#"user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("browser.startup.firstrunSkipsHomepage", true);
user_pref("browser.startup.page", 0);
user_pref("browser.startup.upgradeDialog.enabled", false);
user_pref("browser.aboutwelcome.enabled", false);
user_pref("trailhead.firstrun.didSeeAboutWelcome", true);
user_pref("startup.homepage_welcome_url", "");
user_pref("startup.homepage_welcome_url.additional", "");
user_pref("startup.homepage_override_url", "");
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("datareporting.policy.dataSubmissionPolicyBypassNotification", true);
user_pref("datareporting.healthreport.uploadEnabled", false);
user_pref("toolkit.telemetry.enabled", false);
user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);
user_pref("toolkit.startup.max_resumed_crashes", -1);
user_pref("browser.sessionstore.resume_from_crash", false);
user_pref("browser.tabs.warnOnClose", false);
user_pref("browser.warnOnQuit", false);
user_pref("browser.newtabpage.enabled", false);
user_pref("browser.uitour.enabled", false);
user_pref("browser.discovery.enabled", false);
user_pref("browser.messaging-system.whatsNewPanel.enabled", false);
user_pref("browser.preferences.moreFromMozilla", false);
user_pref("browser.translations.enable", false);
user_pref("browser.translations.automaticallyPopup", false);
user_pref("app.update.auto", false);
user_pref("app.update.checkInstallTime", false);
user_pref("app.normandy.enabled", false);
user_pref("extensions.update.enabled", false);
user_pref("signon.rememberSignons", false);
user_pref("network.captive-portal-service.enabled", false);
user_pref("network.connectivity-service.enabled", false);
user_pref("network.proxy.type", 0);
user_pref("privacy.sanitize.sanitizeOnShutdown", false);
"#;

/// LibreOffice: no Tip of the Day or first-run dialog in a fresh user installation.
pub const LIBREOFFICE_XCU: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<item oor:path="/org.openoffice.Office.Common/Misc"><prop oor:name="ShowTipOfTheDay" oor:op="fuse"><value>false</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Misc"><prop oor:name="FirstRun" oor:op="fuse"><value>false</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Misc"><prop oor:name="CrashReport" oor:op="fuse"><value>false</value></prop></item>
<item oor:path="/org.openoffice.Office.Recovery/RecoveryInfo"><prop oor:name="Enabled" oor:op="fuse"><value>false</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/AutoCorrect"><prop oor:name="SingleQuoteAtStart" oor:op="fuse"><value>0</value></prop></item>
</oor:items>
"#;

/// A launched app; killed (with the processes it spawned) and its profile removed on drop.
pub struct Running {
    pub spec: AppSpec,
    child: Option<Child>,
    /// A unique string on the command line of every process of this launch (the profile
    /// path), for `pkill -f` on Linux and macOS.
    marker: Option<String>,
    /// Image names to kill on Windows / process names on macOS (single-instance apps).
    images: Vec<&'static str>,
    pub profile: Option<PathBuf>,
    pub log: Option<PathBuf>,
}

impl Running {
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// Whether the process we started is still running (`None` when started through
    /// LaunchServices).
    pub fn alive(&mut self) -> Option<bool> {
        self.child
            .as_mut()
            .map(|c| matches!(c.try_wait(), Ok(None)))
    }

    pub fn log_tail(&self) -> String {
        let Some(p) = &self.log else {
            return String::new();
        };
        let text = std::fs::read_to_string(p).unwrap_or_default();
        // Without the noise of a runner that has no system bus.
        let lines: Vec<&str> = text
            .lines()
            .rev()
            .filter(|l| !l.contains("Failed to connect to the bus") && !l.trim().is_empty())
            .take(6)
            .collect();
        lines.into_iter().rev().collect::<Vec<_>>().join(" | ")
    }

    pub fn kill(&mut self) {
        let t = Duration::from_secs(15);
        if let Some(c) = self.child.as_mut() {
            if cfg!(windows) {
                let pid = c.id().to_string();
                let _ = run_out("taskkill", &["/F", "/T", "/PID", &pid], t);
            }
            let _ = c.kill();
            let _ = c.wait();
        }
        if !cfg!(windows)
            && let Some(m) = &self.marker
        {
            let _ = run_out("pkill", &["-KILL", "-f", m], t);
        }
        if self.images.contains(&"Safari") {
            // Quit, so it does not restore this page next time, then forget its saved windows.
            let _ = run_out(
                "/usr/bin/osascript",
                &["-e", "tell application \"Safari\" to quit"],
                Duration::from_secs(5),
            );
            if let Some(home) = std::env::var_os("HOME") {
                let home = PathBuf::from(home);
                for d in [
                    home.join("Library")
                        .join("Saved Application State")
                        .join("com.apple.Safari.savedState"),
                    home.join("Library")
                        .join("Containers")
                        .join("com.apple.Safari")
                        .join("Data")
                        .join("Library")
                        .join("Saved Application State")
                        .join("com.apple.Safari.savedState"),
                ] {
                    let _ = std::fs::remove_dir_all(d);
                }
            }
        }
        for img in &self.images {
            if cfg!(windows) {
                let _ = run_out("taskkill", &["/F", "/T", "/IM", img], t);
            } else {
                let _ = run_out("pkill", &["-KILL", "-x", img], t);
            }
        }
        self.child = None;
        // Let the processes release the profile before it is removed.
        std::thread::sleep(Duration::from_millis(500));
        if let Some(p) = self.profile.take() {
            for _ in 0..10 {
                if std::fs::remove_dir_all(&p).is_ok() || !p.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.kill();
    }
}

static LOG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Where app logs go (the uploaded output dir); `dir` of the launch otherwise.
pub fn set_log_dir(dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    let _ = LOG_DIR.set(dir);
}

fn log_file(dir: &Path, name: &str) -> (Stdio, Stdio, PathBuf) {
    let dir = LOG_DIR.get().map(PathBuf::as_path).unwrap_or(dir);
    let p = dir.join(format!("{name}-{}.log", unique()));
    match std::fs::File::create(&p) {
        Ok(f) => {
            let e = f
                .try_clone()
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null());
            (Stdio::from(f), e, p)
        }
        Err(_) => (Stdio::null(), Stdio::null(), p),
    }
}

fn spawn(mut cmd: Command, dir: &Path, name: &str) -> Result<(Child, PathBuf), String> {
    let (o, e, log) = log_file(dir, name);
    let child = cmd
        .stdin(Stdio::null())
        .stdout(o)
        .stderr(e)
        .spawn()
        .map_err(|e| format!("cannot start {:?}: {e}", cmd.get_program()))?;
    Ok((child, log))
}

/// macOS: the executable inside an `.app` bundle (`CFBundleExecutable`, else the bundle name).
pub fn bundle_exe(app: &Path) -> PathBuf {
    let plist = app.join("Contents").join("Info.plist");
    let name = run_out(
        "/usr/bin/plutil",
        &[
            "-extract",
            "CFBundleExecutable",
            "raw",
            &plist.to_string_lossy(),
        ],
        Duration::from_secs(10),
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| {
        app.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    app.join("Contents").join("MacOS").join(name)
}

/// macOS: `open -a <app> [args]` through LaunchServices, started without waiting for it (it
/// can block on a slow app start); the child is reaped when the app is killed.
fn spawn_open(app: &Path, args: &[&str]) -> Result<Child, String> {
    Command::new("/usr/bin/open")
        .arg("-a")
        .arg(app)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run open -a {}: {e}", app.display()))
}

/// macOS: a new instance of `app` through LaunchServices with `args` passed to it (`open -n -a
/// <app> --args ...`), not waited for. A browser started as a child of this background test
/// process is not allowed to become frontmost; one started by LaunchServices is, as for a user.
/// The browser is found again by its profile path (the run's marker) when it is killed.
fn spawn_open_new(app: &Path, args: &[String]) -> Result<Child, String> {
    Command::new("/usr/bin/open")
        .arg("-n")
        .arg("-a")
        .arg(app)
        .arg("--args")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run open -n -a {}: {e}", app.display()))
}

/// Start a browser on `url` with a fresh profile under `dir`.
pub fn launch_browser(
    spec: AppSpec,
    exe: &Path,
    url: &str,
    dir: &Path,
    force_a11y: bool,
) -> Result<Running, String> {
    let Kind::Browser(b) = spec.kind else {
        return Err("not a browser".into());
    };
    let profile = dir.join(format!("profile-{}", unique()));
    std::fs::create_dir_all(&profile).map_err(|e| format!("profile dir: {e}"))?;
    let marker = profile.to_string_lossy().into_owned();
    let mut run = Running {
        spec,
        child: None,
        marker: Some(marker),
        images: Vec::new(),
        profile: Some(profile.clone()),
        log: None,
    };
    match b {
        Browser::Chrome | Browser::Edge => {
            let args = chromium_args(b == Browser::Edge, url, &profile, force_a11y);
            if cfg!(target_os = "macos") {
                run.child = Some(spawn_open_new(exe, &args)?);
            } else {
                let mut cmd = Command::new(exe);
                cmd.args(&args);
                let (c, log) = spawn(cmd, dir, spec.id)?;
                run.child = Some(c);
                run.log = Some(log);
            }
        }
        Browser::Firefox => {
            std::fs::write(profile.join("user.js"), FIREFOX_USER_JS)
                .map_err(|e| format!("user.js: {e}"))?;
            let mut args: Vec<String> = vec![
                "-no-remote".into(),
                "-new-instance".into(),
                "-profile".into(),
                profile.to_string_lossy().into_owned(),
            ];
            if cfg!(windows) {
                // The process we start stays the browser (not a launcher that exits), so it
                // can be killed with its tree.
                args.push("-wait-for-browser".into());
                run.images = vec!["firefox.exe"];
            }
            args.push(url.into());
            if cfg!(target_os = "macos") {
                run.child = Some(spawn_open_new(exe, &args)?);
                return Ok(run);
            }
            let mut cmd = Command::new(exe);
            cmd.args(&args)
                .env("MOZ_CRASHREPORTER_DISABLE", "1")
                .env("MOZ_DISABLE_AUTO_SAFE_MODE", "1");
            if cfg!(target_os = "linux") {
                cmd.env("GNOME_ACCESSIBILITY", "1")
                    .env("MOZ_ENABLE_WAYLAND", "0");
            }
            let (c, log) = spawn(cmd, dir, spec.id)?;
            run.child = Some(c);
            run.log = Some(log);
        }
        Browser::Safari => {
            // Safari has no profile switch; it is quit (killed) after each page.
            run.images = vec!["Safari"];
            run.marker = None;
            run.child = Some(spawn_open(exe, &[url])?);
        }
    }
    Ok(run)
}

/// Start a native editor on `doc` (a copy of the seed file). Notes takes no document.
pub fn launch_native(spec: AppSpec, exe: &Path, doc: &Path, dir: &Path) -> Result<Running, String> {
    let Kind::Native(n) = spec.kind else {
        return Err("not a native editor".into());
    };
    let mut run = Running {
        spec,
        child: None,
        marker: None,
        images: Vec::new(),
        profile: None,
        log: None,
    };
    let doc_s = doc.to_string_lossy().into_owned();
    match n {
        Native::TextEdit => {
            run.images = vec!["TextEdit"];
            run.child = Some(spawn_open(exe, &[&doc_s])?);
        }
        Native::Notes => {
            run.images = vec!["Notes"];
            run.child = Some(spawn_open(exe, &[])?);
        }
        Native::Notepad | Native::WordPad => {
            run.images = if n == Native::Notepad {
                vec!["notepad.exe"]
            } else {
                vec!["wordpad.exe"]
            };
            let mut cmd = Command::new(exe);
            cmd.arg(doc);
            let (c, log) = spawn(cmd, dir, spec.id)?;
            run.child = Some(c);
            run.log = Some(log);
        }
        Native::Gedit | Native::GnomeTextEditor => {
            let mut cmd = Command::new(exe);
            if n == Native::Gedit {
                cmd.arg("--new-window");
            } else {
                cmd.arg("--standalone");
            }
            cmd.arg(doc).env("GTK_A11Y", "atspi");
            run.marker = Some(doc_s.clone());
            let (c, log) = spawn(cmd, dir, spec.id)?;
            run.child = Some(c);
            run.log = Some(log);
        }
        Native::Writer => {
            let profile = dir.join(format!("lo-profile-{}", unique()));
            let user = profile.join("user");
            std::fs::create_dir_all(&user).map_err(|e| format!("profile dir: {e}"))?;
            std::fs::write(user.join("registrymodifications.xcu"), LIBREOFFICE_XCU)
                .map_err(|e| format!("registrymodifications.xcu: {e}"))?;
            let mut cmd = Command::new(exe);
            cmd.args([
                format!("-env:UserInstallation={}", file_url(&profile)),
                "--writer".into(),
                "--norestore".into(),
                "--nologo".into(),
                "--nolockcheck".into(),
                "--nodefault".into(),
            ])
            .arg(doc)
            .env("SAL_USE_VCLPLUGIN", "gtk3");
            run.marker = Some(profile.to_string_lossy().into_owned());
            run.profile = Some(profile);
            let (c, log) = spawn(cmd, dir, spec.id)?;
            run.child = Some(c);
            run.log = Some(log);
        }
    }
    Ok(run)
}

fn unique() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

/// Bring the app to the front: its entry in `apps()` (by pid, else by name), activated through
/// `open_app` with that exact name (never a launch). Returns what happened, for the log.
pub fn activate(cu: &dyn ComputerUse, run: &Running) -> String {
    let apps = match cu.apps() {
        Ok(a) => a,
        Err(e) => return format!("apps() failed: {e}"),
    };
    let pid = run.pid();
    let names = run.spec.app_names();
    let entry = apps
        .iter()
        .find(|a| pid.is_some() && a.pid == pid)
        .or_else(|| {
            apps.iter().find(|a| {
                let n = a.name.to_lowercase();
                let id = a.id.clone().unwrap_or_default().to_lowercase();
                names.iter().any(|x| n.contains(x) || id.contains(x))
            })
        });
    let Some(e) = entry else {
        let seen: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        return format!("not among running apps {seen:?}");
    };
    if e.frontmost {
        return format!("{} already frontmost", e.name);
    }
    let name = e.id.clone().unwrap_or_else(|| e.name.clone());
    match cu.open_app(&name) {
        Ok(o) => format!("open_app({name}): verified={}", o.verified),
        Err(err) => match cu.open_app(&e.name) {
            Ok(o) => format!("open_app({}): verified={}", e.name, o.verified),
            Err(e2) => format!("open_app({name}) failed: {err}; by name: {e2}"),
        },
    }
}

/// A screenshot of the screen (macOS, Windows) or the focused window (Linux) to `path`, best
/// effort.
pub fn screenshot(cu: &dyn ComputerUse, path: &Path) -> Result<(), String> {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let t = Duration::from_secs(20);
    let p = path.to_string_lossy().into_owned();
    if cfg!(target_os = "macos") {
        return run_out("/usr/sbin/screencapture", &["-x", &p], t).map(|_| ());
    }
    if cfg!(windows) {
        let script = "Add-Type -AssemblyName System.Windows.Forms, System.Drawing; \
             $b = [System.Windows.Forms.SystemInformation]::VirtualScreen; \
             $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height; \
             $g = [System.Drawing.Graphics]::FromImage($bmp); \
             $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size); \
             $bmp.Save($env:NUGGET_SHOT)";
        return run_out_env(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", script],
            &[("NUGGET_SHOT", &p)],
            t,
        )
        .map(|_| ());
    }
    match cu.screenshot(&nugget_a11y::WindowSel::Focused) {
        Ok(Some(png)) => std::fs::write(path, png).map_err(|e| e.to_string()),
        Ok(None) => Err("no screenshot support".into()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_lists() {
        let apps = all_apps();
        let ids: Vec<&str> = apps.iter().map(|a| a.id).collect();
        for b in ["chrome", "edge", "firefox"] {
            assert!(ids.contains(&b), "{ids:?}");
        }
        assert_eq!(cfg!(target_os = "macos"), ids.contains(&"safari"));
        assert_eq!(cfg!(windows), ids.contains(&"wordpad"));
        assert_eq!(cfg!(target_os = "linux"), ids.contains(&"writer"));
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "unique ids");
    }

    #[test]
    fn browser_args() {
        let profile = std::env::temp_dir().join("p");
        let a = chromium_args(
            true,
            "http://127.0.0.1:1/index.html?editor=quill",
            &profile,
            false,
        );
        assert!(a.iter().any(|x| x.starts_with("--user-data-dir=")));
        assert!(a.contains(&"--no-first-run".to_string()));
        assert!(a.contains(&"--edge-skip-compat-layer-relaunch".to_string()));
        assert!(!a.iter().any(|x| x == "--force-renderer-accessibility"));
        assert_eq!(
            a.last().unwrap(),
            "http://127.0.0.1:1/index.html?editor=quill"
        );
        let a = chromium_args(false, "u", &profile, true);
        assert!(a.iter().any(|x| x == "--force-renderer-accessibility"));
        assert!(!a.iter().any(|x| x.contains("compat-layer")));
        assert!(FIREFOX_USER_JS.lines().all(|l| l.starts_with("user_pref(")));
    }

    #[test]
    fn urls_and_processes() {
        assert_eq!(
            file_url(Path::new("/tmp/a b")),
            "file:///tmp/a%20b".to_string()
        );
        assert_eq!(file_url(Path::new(r"C:\x\y")), "file:///C:/x/y");
        let r = run_out(
            if cfg!(windows) { "cmd" } else { "sh" },
            if cfg!(windows) {
                &["/c", "echo hi"]
            } else {
                &["-c", "echo hi"]
            },
            Duration::from_secs(20),
        );
        assert_eq!(r.unwrap().trim(), "hi");
        assert!(run_out("nugget-no-such-program", &[], Duration::from_secs(1)).is_err());
        if !cfg!(windows) {
            let t = Instant::now();
            assert!(run_out("sleep", &["30"], Duration::from_millis(300)).is_err());
            assert!(t.elapsed() < Duration::from_secs(10));
        }
    }
}
