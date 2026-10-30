//! The `native` environment: the apps on the machine's own desktop, read and driven through its
//! real accessibility API: UI Automation on Windows, AX on macOS. The Linux equivalent is `xvfb`.
//!
//! - **Windows.** The apps are served on the loopback socket as in `xvfb`, and each app gets one
//!   Microsoft Edge window in app mode (`--app=<url>`, renderer accessibility forced on, a
//!   throwaway profile, tiled like the Xvfb desktop). Edge ships with Windows, so nothing is
//!   installed. Occlusion and background throttling are off, so a covered window still reloads
//!   when the world changes. The run waits until every window has loaded its page and is visible
//!   to UI Automation; every process is killed on drop.
//! - **macOS.** The same, with one Google Chrome app window per app, read and driven over AX
//!   (see [`super::env_ax`]). The process running the bench needs the Accessibility permission.
//! - **Session** ([`NativeSession`]): observations and confirmations go to the harness as in
//!   `socket`, but every action is performed on the real window through UI Automation by
//!   [`super::env_uia::UiaDriver`] (over AX on macOS); the page posts it to the server, exactly
//!   as a user's input.
//!   So the `reference` agent drives the real windows too, which checks the harness.
//! - **The nugget arm** gets [`computer`]: a `ComputerUse` that reads the real windows over UI
//!   Automation and presents them with bench ids (see `env_uia`). It is available while a native
//!   desktop is up, on the thread that started it ([`NativeDesktop`] registers its driver there
//!   and unregisters it on drop).
//!
//! `NUGGET_CUA_EDGE` (Windows) and `NUGGET_CUA_CHROME` (macOS) override the browser (any
//! Chromium build with `--app` works).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nugget_a11y::computer::ComputerUse;
use nugget_policy::ActionDescriptor;

use super::agent::{ActResult, EnvError, Session};
use super::apps::APP_IDS;
use super::env_ax;
use super::env_uia::{UiaComputer, UiaDriver};
use super::nugget_env::WorldPort;
use super::server::Stage;
use super::session::EpisodeSession;
use super::ui::{Action, Node, Observation};

/// Window size and tiling, as on the Xvfb desktop.
const WINDOW: (u32, u32) = (530, 490);

/// What the native desktop came up with, for the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeInfo {
    pub browser: String,
    /// App → the process that owns its window (when UI Automation reported it).
    pub pids: HashMap<String, u32>,
    pub accessibility: &'static str,
}

thread_local! {
    /// The live native desktop's driver, on the thread that runs the tasks (agents are run on
    /// the thread that started the desktop).
    static CURRENT: RefCell<Option<Arc<UiaDriver>>> = const { RefCell::new(None) };
}

/// Make `driver` the backend of [`computer`] on this thread until the guard drops.
pub fn register(driver: Arc<UiaDriver>) -> Registration {
    CURRENT.with(|c| *c.borrow_mut() = Some(driver));
    Registration(())
}

/// Unregisters the native driver on drop.
pub struct Registration(());

impl Drop for Registration {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = None);
    }
}

/// The computer-use backend over the real windows, while a native desktop is up on this thread.
pub fn computer(port: Arc<dyn WorldPort>) -> Option<Arc<dyn ComputerUse>> {
    let driver = CURRENT.with(|c| c.borrow().clone())?;
    Some(Arc::new(UiaComputer::new(port, driver)))
}

/// `NUGGET_CUA_EDGE`, then Edge in its usual install locations, then `msedge` on `PATH`.
pub fn find_edge() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NUGGET_CUA_EDGE") {
        return Some(PathBuf::from(p));
    }
    let rel = Path::new("Microsoft")
        .join("Edge")
        .join("Application")
        .join("msedge.exe");
    for var in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            let p = PathBuf::from(base).join(&rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .flat_map(|d| [d.join("msedge.exe"), d.join("msedge")])
            .find(|p| p.is_file())
    })
}

/// The browser for this OS's app windows: Edge on Windows, Chrome on macOS.
pub fn find_browser() -> Result<PathBuf, String> {
    if cfg!(windows) {
        find_edge()
            .ok_or_else(|| "Microsoft Edge not found (set NUGGET_CUA_EDGE to msedge.exe)".into())
    } else if cfg!(target_os = "macos") {
        env_ax::find_chrome().ok_or_else(|| {
            "Google Chrome not found in /Applications (set NUGGET_CUA_CHROME to its binary)".into()
        })
    } else {
        Err(format!(
            "the native environment needs Windows (UI Automation and Microsoft Edge) or macOS \
             (AX and Google Chrome); this is {}. On Linux use --cua-env xvfb",
            std::env::consts::OS
        ))
    }
}

/// Whether the prerequisites for the `native` environment are present.
pub fn available() -> Result<(), String> {
    find_browser().map(|_| ())
}

/// Edge's command line for one app window (Windows).
fn edge_args(url: &str, profile: &Path, pos: (u32, u32), size: (u32, u32)) -> Vec<String> {
    vec![
        format!("--app={url}"),
        format!("--user-data-dir={}", profile.display()),
        format!("--window-position={},{}", pos.0, pos.1),
        format!("--window-size={},{}", size.0, size.1),
        "--force-renderer-accessibility".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-sync".into(),
        "--disable-extensions".into(),
        "--disable-component-update".into(),
        "--disable-background-networking".into(),
        "--disable-features=Translate,msEdgeTranslate,CalculateNativeWinOcclusion,\
         msUndersideButton,msEdgeSidebarV2"
            .into(),
        "--disable-backgrounding-occluded-windows".into(),
        "--disable-renderer-backgrounding".into(),
        "--disable-background-timer-throttling".into(),
        "--edge-skip-compat-layer-relaunch".into(),
        "--no-proxy-server".into(),
    ]
}

fn is_edge(name: &str) -> bool {
    name.to_lowercase().contains("msedge")
}

/// `NUGGET_CUA_DUMP`: write each browser window's real tree (normalised as the matcher reads
/// it) to `<dir>/<n>.json`, for diagnosing a match. Best effort.
fn dump_windows(sys: &dyn ComputerUse, dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    for (i, w) in sys.windows().unwrap_or_default().iter().enumerate() {
        let Ok(mut tree) = sys.snapshot(
            &nugget_a11y::computer::WindowSel::Id(w.id.clone()),
            &super::env_uia::real_limits(),
        ) else {
            continue;
        };
        env_ax::normalize(&mut tree.root);
        if let Ok(json) = serde_json::to_string_pretty(&tree) {
            let _ = std::fs::write(dir.join(format!("{i}.json")), json);
        }
    }
}

/// One browser window per app, and the accessibility driver over them.
pub struct NativeDesktop {
    pub info: NativeInfo,
    pub driver: Arc<UiaDriver>,
    children: Vec<(String, Child)>,
    dir: tempfile::TempDir,
    registration: Option<Registration>,
}

impl NativeDesktop {
    fn log(&self, name: &str) -> Stdio {
        std::fs::File::create(self.dir.path().join(format!("{name}.log")))
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null())
    }

    fn log_tail(&self, name: &str) -> String {
        let text = std::fs::read_to_string(self.dir.path().join(format!("{name}.log")))
            .unwrap_or_default();
        let lines: Vec<&str> = text.lines().rev().take(8).collect();
        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    /// Open the app windows and wait until each has loaded its page and is visible to the
    /// accessibility API. `stage` must hold a world (the pages render it).
    pub fn start(base_url: &str, stage: &Stage, timeout: Duration) -> Result<Self, String> {
        let browser = find_browser()?;
        let mac = cfg!(target_os = "macos");
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let sys = nugget_a11y::system_computer();
        if sys.permission() == nugget_a11y::Permission::Denied {
            return Err(
                "the accessibility permission is not granted to this process (macOS: System \
                 Settings, Privacy & Security, Accessibility)"
                    .into(),
            );
        }
        let (accessibility, kind) = if mac {
            ("ax", "chrome")
        } else {
            ("uia", "edge")
        };
        let mut d = NativeDesktop {
            info: NativeInfo {
                browser: browser.display().to_string(),
                pids: HashMap::new(),
                accessibility,
            },
            driver: Arc::new(UiaDriver::new(
                sys.clone(),
                Arc::new(HashMap::<String, u32>::new()),
            )),
            children: Vec::new(),
            dir,
            registration: None,
        };
        let mut spawned: HashMap<String, u32> = HashMap::new();
        for (i, app) in APP_IDS.iter().enumerate() {
            let (x, y) = ((i as u32 % 3) * WINDOW.0, (i as u32 / 3) * WINDOW.1);
            let profile = d.dir.path().join(format!("profile-{app}"));
            let url = format!("{base_url}/{app}/");
            let args = if mac {
                env_ax::chrome_args(&url, &profile, (x, y), WINDOW)
            } else {
                edge_args(&url, &profile, (x, y), WINDOW)
            };
            let mut cmd = Command::new(&browser);
            cmd.args(args);
            let name = format!("{kind}-{app}");
            let child = cmd
                .stdin(Stdio::null())
                .stdout(d.log(&name))
                .stderr(d.log(&format!("{name}.err")))
                .spawn()
                .map_err(|e| format!("cannot start {}: {e}", browser.display()))?;
            spawned.insert((*app).to_owned(), child.id());
            d.children.push((name, child));
        }

        // Every page served, and every window visible to UI Automation.
        let t0 = Instant::now();
        loop {
            let served = stage.pages_served();
            let ws = sys.windows().unwrap_or_default();
            let found: HashMap<String, u32> = spawned
                .iter()
                .filter(|(_, pid)| ws.iter().any(|w| w.app.pid == Some(**pid)))
                .map(|(app, pid)| (app.clone(), *pid))
                .collect();
            let titled = ws
                .iter()
                .filter(|w| {
                    let is = if mac { env_ax::is_chrome } else { is_edge };
                    is(&w.app.name) || w.app.id.as_deref().is_some_and(is)
                })
                .count();
            let all_served = APP_IDS.iter().all(|a| served.contains(*a));
            if all_served && (found.len() == APP_IDS.len() || titled >= APP_IDS.len()) {
                // Map by process where Edge kept the process we started; by title otherwise.
                d.info.pids = found;
                break;
            }
            if t0.elapsed() > timeout {
                let missing: Vec<&&str> =
                    APP_IDS.iter().filter(|a| !served.contains(**a)).collect();
                let titles: Vec<String> = ws
                    .iter()
                    .map(|w| format!("{} {:?} pid {:?}", w.app.name, w.title, w.app.pid))
                    .collect();
                return Err(format!(
                    "app windows did not come up within {timeout:?} (pages not served: \
                     {missing:?}; windows: {titles:?}); {kind} says: {}",
                    d.log_tail(&format!("{kind}-mail.err"))
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if let Some(dir) = std::env::var_os("NUGGET_CUA_DUMP") {
            dump_windows(&*sys, Path::new(&dir));
        }
        d.driver = Arc::new(UiaDriver::new(sys, Arc::new(d.info.pids.clone())));
        d.registration = Some(register(d.driver.clone()));
        Ok(d)
    }

    /// Where the browser logs are written (kept until drop).
    pub fn log_dir(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for NativeDesktop {
    fn drop(&mut self) {
        self.registration.take();
        let n = |c: &std::sync::atomic::AtomicU32| c.load(std::sync::atomic::Ordering::Relaxed);
        eprintln!(
            "cua native: {} real window reads, {} did not settle, {} actions needed the keyboard",
            n(&self.driver.reads),
            n(&self.driver.unsettled),
            n(&self.driver.fallbacks)
        );
        for (_, c) in self.children.iter_mut().rev() {
            let _ = c.kill();
            let _ = c.wait();
        }
        // The browser's helper processes let go of the profile shortly after it exits.
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// How long a freshly served page gets to replace the old document.
const PAGE_COMMIT: Duration = Duration::from_millis(400);
/// Longest wait for a page to reload after the world changed.
const PAGE_RELOAD: Duration = Duration::from_secs(6);

/// Wait until `app`'s window shows a page served at the current world version (pages reload
/// whenever the world changes, a confirmation included): an input sent to a page that is about
/// to reload would be lost with it. Bounded; on timeout the action goes ahead.
fn wait_current_page(stage: &Stage, app: &str) {
    let t0 = Instant::now();
    while t0.elapsed() < PAGE_RELOAD {
        let Some((v, at)) = stage.page_served(app) else {
            // No page shows this app (no display).
            return;
        };
        if v == stage.version() {
            let age = at.elapsed();
            if age >= PAGE_COMMIT {
                return;
            }
            std::thread::sleep(PAGE_COMMIT - age);
            continue;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// A task's session in the native environment: the harness session for observing and
/// confirming, real UI Automation for every action (see the module docs).
pub struct NativeSession<'a> {
    inner: &'a mut EpisodeSession,
    stage: Arc<Stage>,
    driver: Arc<UiaDriver>,
    max_steps: u32,
    steps: u32,
}

impl<'a> NativeSession<'a> {
    pub fn new(
        inner: &'a mut EpisodeSession,
        stage: Arc<Stage>,
        driver: Arc<UiaDriver>,
        max_steps: u32,
    ) -> Self {
        Self {
            inner,
            stage,
            driver,
            max_steps,
            steps: 0,
        }
    }
}

impl Session for NativeSession<'_> {
    fn observe(&mut self) -> Result<Observation, EnvError> {
        self.inner.observe()
    }

    fn act(&mut self, action: &Action) -> Result<ActResult, EnvError> {
        if self.steps >= self.max_steps {
            return Err(EnvError::StepBudget(self.max_steps));
        }
        self.steps += 1;
        wait_current_page(&self.stage, action.app());
        let obs = self.inner.observe()?;
        let stage = self.stage.clone();
        let app = action.app().to_owned();
        let bench = move || -> nugget_core::Result<Node> {
            stage
                .with(|w| w.window(&app))
                .map_err(nugget_core::Error::Other)?
                .ok_or_else(|| nugget_core::Error::NotFound(format!("`{app}` is not open")))
        };
        let stage = self.stage.clone();
        let version = move || stage.version();
        let r = self.driver.perform(action, &obs, &version, &bench);
        if !r.ok {
            eprintln!(
                "cua native: {action:?} failed: {}",
                r.message.as_deref().unwrap_or("-")
            );
            if let Some(dir) = std::env::var_os("NUGGET_CUA_DUMP") {
                let at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or_default();
                dump_windows(
                    &**self.driver.sys(),
                    &Path::new(&dir).join(format!("fail-{at}")),
                );
            }
        }
        Ok(r)
    }

    fn confirm(&mut self, action: &ActionDescriptor) -> Result<bool, EnvError> {
        self.inner.confirm(action)
    }

    fn record_usage(&mut self, input_tokens: u64, output_tokens: u64) {
        self.inner.record_usage(input_tokens, output_tokens);
    }

    fn steps_taken(&self) -> u32 {
        self.steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_on_linux_with_a_clear_reason() {
        if cfg!(windows) || cfg!(target_os = "macos") {
            return;
        }
        let e = available().unwrap_err();
        assert!(e.contains("needs Windows"), "{e}");
        assert!(e.contains("macOS"), "{e}");
        assert!(e.contains("xvfb"), "{e}");
    }

    #[test]
    fn edge_app_window_command_line() {
        let a = edge_args(
            "http://127.0.0.1:9/mail/",
            Path::new("p"),
            (0, 490),
            (530, 490),
        );
        assert_eq!(a[0], "--app=http://127.0.0.1:9/mail/");
        assert!(a.contains(&"--window-position=0,490".to_owned()));
        assert!(a.contains(&"--force-renderer-accessibility".to_owned()));
        assert!(is_edge("msedge.exe"));
        assert!(!is_edge("Google Chrome"));
    }

    #[test]
    fn no_backend_without_a_desktop() {
        struct Never;
        impl WorldPort for Never {
            fn observe(&self) -> Result<Observation, EnvError> {
                Err(EnvError::Transport("never".into()))
            }
            fn act(&self, _: &Action) -> Result<ActResult, EnvError> {
                Err(EnvError::Transport("never".into()))
            }
        }
        assert!(computer(Arc::new(Never)).is_none());
    }
}
