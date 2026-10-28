//! The `xvfb` environment: a real Linux desktop for the apps.
//!
//! Starts, in order: Xvfb on a free display (or reuses `NUGGET_CUA_DISPLAY`, e.g. one started by
//! `bench/cua/env/desktop.sh`), a session D-Bus, the AT-SPI bus, a window manager, and one kiosk
//! Chromium window per app (`--app=` mode, renderer accessibility forced on, so every element is
//! visible to AT-SPI with the same role, name and id as in the headless tree). Every process is
//! killed on drop. D-Bus, AT-SPI and the window manager are optional: when missing, the run still
//! works and the report records what was available.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::apps::APP_IDS;
use super::server::Stage;

const SCREEN: (u32, u32) = (1600, 1000);
const WINDOW: (u32, u32) = (530, 490);
const WINDOW_MANAGERS: &[&str] = &[
    "openbox",
    "matchbox-window-manager",
    "fluxbox",
    "icewm",
    "xfwm4",
    "metacity",
];
const ATSPI_LAUNCHERS: &[&str] = &[
    "/usr/libexec/at-spi-bus-launcher",
    "/usr/lib/at-spi2-core/at-spi-bus-launcher",
    "/usr/lib/at-spi2/at-spi-bus-launcher",
    "/usr/lib/x86_64-linux-gnu/at-spi-bus-launcher",
];

/// What the desktop came up with, for the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopInfo {
    pub display: String,
    pub reused_display: bool,
    pub window_manager: Option<String>,
    pub atspi: bool,
    pub chromium: String,
}

pub struct Desktop {
    pub info: DesktopInfo,
    children: Vec<(String, Child)>,
    dir: tempfile::TempDir,
}

/// Find an executable on `PATH` (or check an absolute path).
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return p.is_file().then_some(p);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// `NUGGET_CUA_CHROMIUM`, then Chromium or Chrome on `PATH`, then Playwright's bundled build.
pub fn find_chromium() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NUGGET_CUA_CHROMIUM") {
        return Some(PathBuf::from(p));
    }
    for n in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
    ] {
        if let Some(p) = which(n) {
            return Some(p);
        }
    }
    let root = Path::new("/opt/pw-browsers");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("chromium-"))
        })
        .collect();
    dirs.sort();
    dirs.iter().rev().find_map(|d| {
        ["chrome-linux/chrome", "chrome-linux64/chrome"]
            .iter()
            .map(|c| d.join(c))
            .find(|p| p.is_file())
    })
}

/// Whether the prerequisites for the `xvfb` environment are present.
pub fn available() -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("the xvfb environment is Linux-only".into());
    }
    if std::env::var_os("NUGGET_CUA_DISPLAY").is_none() && which("Xvfb").is_none() {
        return Err("Xvfb not found (install xvfb, or set NUGGET_CUA_DISPLAY)".into());
    }
    if find_chromium().is_none() {
        return Err("Chromium not found (install chromium, or set NUGGET_CUA_CHROMIUM)".into());
    }
    Ok(())
}

fn running_as_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1).map(|u| u == "0"))
        })
        .unwrap_or(false)
}

fn free_display() -> Option<u32> {
    (90..160).find(|n| {
        !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
            && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
    })
}

impl Desktop {
    fn log(&self, name: &str) -> Stdio {
        std::fs::File::create(self.dir.path().join(format!("{name}.log")))
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null())
    }

    fn spawn(&mut self, name: &str, cmd: &mut Command) -> Result<(), String> {
        let child = cmd
            .stdin(Stdio::null())
            .stdout(self.log(name))
            .stderr(self.log(&format!("{name}.err")))
            .spawn()
            .map_err(|e| format!("cannot start {name}: {e}"))?;
        self.children.push((name.to_owned(), child));
        Ok(())
    }

    fn log_tail(&self, name: &str) -> String {
        let text = std::fs::read_to_string(self.dir.path().join(format!("{name}.log")))
            .unwrap_or_default();
        let lines: Vec<&str> = text.lines().rev().take(8).collect();
        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    /// Bring the desktop up and wait until every app window has loaded its page.
    pub fn start(base_url: &str, stage: &Stage, timeout: Duration) -> Result<Self, String> {
        available()?;
        let chromium = find_chromium().ok_or("Chromium not found")?;
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut d = Desktop {
            info: DesktopInfo {
                display: String::new(),
                reused_display: false,
                window_manager: None,
                atspi: false,
                chromium: chromium.display().to_string(),
            },
            children: Vec::new(),
            dir,
        };
        let mut env: Vec<(String, String)> = Vec::new();

        // 1. X server.
        if let Some(disp) = std::env::var("NUGGET_CUA_DISPLAY")
            .ok()
            .filter(|s| !s.is_empty())
        {
            d.info.display = disp;
            d.info.reused_display = true;
        } else {
            let n = free_display().ok_or("no free X display number")?;
            d.info.display = format!(":{n}");
            d.spawn(
                "xvfb",
                Command::new("Xvfb").args([
                    d.info.display.as_str(),
                    "-screen",
                    "0",
                    &format!("{}x{}x24", SCREEN.0, SCREEN.1),
                    "-nolisten",
                    "tcp",
                ]),
            )?;
            let sock = PathBuf::from(format!("/tmp/.X11-unix/X{n}"));
            let t0 = Instant::now();
            while !sock.exists() {
                if t0.elapsed() > Duration::from_secs(10) {
                    return Err(format!("Xvfb did not start: {}", d.log_tail("xvfb.err")));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        env.push(("DISPLAY".into(), d.info.display.clone()));

        // 2. Session bus and 3. AT-SPI bus (skipped when reusing a prepared display).
        let mut bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
        if !d.info.reused_display && which("dbus-daemon").is_some() {
            d.spawn(
                "dbus",
                Command::new("dbus-daemon").args(["--session", "--nofork", "--print-address=1"]),
            )?;
            let log = d.dir.path().join("dbus.log");
            let t0 = Instant::now();
            bus = loop {
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                if let Some(l) = text.lines().next().filter(|l| !l.is_empty()) {
                    break Some(l.trim().to_owned());
                }
                if t0.elapsed() > Duration::from_secs(5) {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
        }
        if let Some(b) = &bus {
            env.push(("DBUS_SESSION_BUS_ADDRESS".into(), b.clone()));
            if d.info.reused_display {
                d.info.atspi = true;
            } else if let Some(l) = ATSPI_LAUNCHERS.iter().find(|p| Path::new(p).is_file()) {
                d.spawn(
                    "atspi",
                    Command::new(l)
                        .arg("--launch-immediately")
                        .envs(env.iter().cloned()),
                )?;
                d.info.atspi = true;
            }
        }
        env.push(("ACCESSIBILITY_ENABLED".into(), "1".into()));
        env.push(("NO_AT_BRIDGE".into(), "0".into()));

        // 4. Window manager.
        if !d.info.reused_display
            && let Some(wm) = WINDOW_MANAGERS.iter().find(|w| which(w).is_some())
        {
            d.spawn("wm", Command::new(wm).envs(env.iter().cloned()))?;
            d.info.window_manager = Some((*wm).to_owned());
        }

        // 5. One kiosk window per app, tiled three across.
        let root = running_as_root();
        for (i, app) in APP_IDS.iter().enumerate() {
            let (x, y) = ((i as u32 % 3) * WINDOW.0, (i as u32 / 3) * WINDOW.1);
            let profile = d.dir.path().join(format!("profile-{app}"));
            let mut cmd = Command::new(&chromium);
            cmd.args([
                format!("--app={base_url}/{app}/"),
                format!("--user-data-dir={}", profile.display()),
                format!("--window-position={x},{y}"),
                format!("--window-size={},{}", WINDOW.0, WINDOW.1),
                "--no-first-run".into(),
                "--no-default-browser-check".into(),
                "--disable-gpu".into(),
                "--disable-dev-shm-usage".into(),
                "--disable-features=Translate,MediaRouter".into(),
                "--password-store=basic".into(),
                "--force-renderer-accessibility".into(),
                "--proxy-server=direct://".into(),
                "--proxy-bypass-list=*".into(),
            ])
            .envs(env.iter().cloned());
            if root {
                cmd.arg("--no-sandbox");
            }
            d.spawn(&format!("chromium-{app}"), &mut cmd)?;
        }

        let t0 = Instant::now();
        loop {
            let served = stage.pages_served();
            if APP_IDS.iter().all(|a| served.contains(*a)) {
                break;
            }
            if t0.elapsed() > timeout {
                let missing: Vec<&&str> =
                    APP_IDS.iter().filter(|a| !served.contains(**a)).collect();
                return Err(format!(
                    "app windows did not load within {timeout:?} (missing {missing:?}); chromium says: {}",
                    d.log_tail("chromium-mail.err")
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(d)
    }

    /// Where child process logs are written (kept until drop).
    pub fn log_dir(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        for (_, c) in self.children.iter_mut().rev() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
