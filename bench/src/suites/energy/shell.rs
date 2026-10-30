//! `shell_idle` and `shell_focus`: the desktop shell (`nugget-desktop`, from [`SHELL_ENV`]),
//! started hidden in the tray (`--minimized`) against a `nuggetd` launched by the bench, metered
//! with [`Meter`] (the shell with its descendants, i.e. its webview processes on Linux; the
//! daemon; for `shell_focus` the local model server) on a real desktop session.
//!
//! - The daemon is the real `nuggetd` ([`find_nuggetd`]) on an empty temporary `NUGGET_HOME`
//!   (`perf_daemon`'s key file and hermetic config: no `[runtime]`, no MCP, no vault). The shell
//!   gets the same `NUGGET_HOME`, so it connects to that daemon's socket and never starts its
//!   own, and an isolated app config directory: Tauri's `app_config_dir` is
//!   `$XDG_CONFIG_HOME/dev.nugget.desktop` on Linux (`XDG_DATA_HOME` and `XDG_CACHE_HOME` are
//!   moved too) and `$HOME/Library/Application Support/dev.nugget.desktop` on macOS (`HOME` is
//!   moved). In it the bench writes `doubletap.json` (Double Tap on; read-ahead on or off) and
//!   `settings.json` (update channel `off`: no update checks, no network).
//! - Settling: with `NUGGET_E2E_PROBE` (honoured by debug builds only) the shell writes a
//!   `read_ahead_loop` line once Double Tap is set up; the bench waits for it (at most
//!   [`PROBE_WAIT`]) and then [`EnergyParams::shell_settle_s`]. A release build writes nothing:
//!   then the [`PROBE_WAIT`] itself is the settle period, and no probe counts are reported.
//! - `shell_idle`: nothing in front but the desktop; one window of [`EnergyParams::idle_s`].
//! - `shell_focus`: the local model is `nuggetd-fake-llama-server` ([`FAKE_SERVER_ENV`]),
//!   started by the bench on a loopback port and configured as the daemon's only provider
//!   (`openai_compatible` on `127.0.0.1`, so on-device: read-ahead is allowed and sends
//!   llama.cpp's `cache_prompt` prefill, which the fake answers at once). That isolates the
//!   shell's and the daemon's costs from a real model's prefill (`prefill` measures that). Three
//!   text-field windows are opened ([`TextWindows`]: a GTK 3 window per field on Linux, three
//!   TextEdit documents on macOS) and a scripted window of [`EnergyParams::focus_s`] switches
//!   between them every [`EnergyParams::focus_every_s`] in a fixed order ([`focus_plan`]). It
//!   runs twice, read-ahead off then on, restarting the shell between the runs (daemon and
//!   server keep running). With a system energy source, the baseline is the same script with
//!   the windows open and nothing of Nugget running.
//!
//! Skipped (recorded in `config.params`, never failed) without [`SHELL_ENV`], without a display
//! (`DISPLAY` on Linux), without the focus tools (`xdotool` and a Python with GTK 3 on Linux,
//! `osascript` on macOS) or the fake server, or on another OS. Linux also needs a session bus
//! with AT-SPI and a window manager, started by the caller: `bench/scripts/energy-linux.sh`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::daemon::{env_for, runtime};
use super::meter::{EnergySource, Meter, Reading, baseline};
use super::{
    EnergyParams, SHELL_ENV, Scenario, energy_metrics, record, round3, skip, wants_baseline,
    window_metrics,
};
use crate::corpus::{Corpus, CorpusConfig};
use crate::report::{Direction, Metric};
use crate::runner::{BenchError, RunContext};
use crate::suites::perf_daemon::{Daemon, Env, find_nuggetd, seed_home, write_config};

/// Environment variable naming the fake `llama-server` (`nuggetd-fake-llama-server`) for
/// `shell_focus`; default: next to `nugget-bench` or `nuggetd`.
pub const FAKE_SERVER_ENV: &str = "NUGGET_BENCH_FAKE_LLAMA_SERVER";
/// Environment variable naming the Python with GTK 3 for the Linux text windows.
pub const PYTHON_ENV: &str = "NUGGET_BENCH_PYTHON";
/// The shell's bundle identifier (`tauri.conf.json`), the last part of its config directory.
const APP_ID: &str = "dev.nugget.desktop";
/// The provider id the fake server is configured under.
const PROVIDER: &str = "bench-local";
/// How long to wait for the shell's probe line (and the settle period of a release build).
const PROBE_WAIT: Duration = Duration::from_secs(30);
/// Upper bound on a helper command (one focus switch, opening windows).
const HELPER_LIMIT: Duration = Duration::from_secs(20);
const DIM: usize = 384;
/// The Linux text window (`bench/scripts/energy-text-window.py`), written to the run's temp dir.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const TEXT_WINDOW_PY: &str = include_str!("../../../scripts/energy-text-window.py");
/// The three text windows: title and the text already in the field.
const WINDOWS: [(&str, &str); 3] = [
    ("Quarterly report", "Hi Alex, the numbers "),
    ("Team offsite", "Sounds good, I can "),
    ("Vendor contract", "Thanks for the draft, "),
];

/// What a run needs that is not always there: the shell, the daemon, and for `shell_focus` the
/// focus driver and the fake server.
struct Found {
    shell: PathBuf,
    daemon: PathBuf,
}

/// The shell and daemon binaries and a desktop session, or why the scenario cannot run.
fn preconditions() -> Result<Found, String> {
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        return Err("the shell scenarios run on Linux (X11) and macOS only".into());
    }
    let shell = match std::env::var_os(SHELL_ENV) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            return Err(format!(
                "{SHELL_ENV} is not set (the nugget-desktop binary to run)"
            ));
        }
    };
    if !shell.is_file() {
        return Err(format!("{SHELL_ENV}={} is not a file", shell.display()));
    }
    if cfg!(target_os = "linux") && std::env::var_os("DISPLAY").is_none_or(|d| d.is_empty()) {
        return Err(
            "no display (DISPLAY is unset; run under bench/scripts/energy-linux.sh)".into(),
        );
    }
    let daemon = find_nuggetd()?;
    Ok(Found { shell, daemon })
}

/// `name` in a `PATH` directory.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// The fake `llama-server`: [`FAKE_SERVER_ENV`], else next to this executable (or one level
/// up) or next to `daemon`.
fn find_fake_server(daemon: &Path) -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(FAKE_SERVER_ENV).filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!("{FAKE_SERVER_ENV}={} is not a file", p.display()))
        };
    }
    let name = format!("nuggetd-fake-llama-server{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().map_err(|e| format!("current executable: {e}"))?;
    let dirs = exe
        .ancestors()
        .skip(1)
        .take(2)
        .chain(daemon.parent())
        .map(Path::to_path_buf);
    let mut tried = Vec::new();
    for d in dirs {
        let c = d.join(&name);
        if c.is_file() {
            return Ok(c);
        }
        tried.push(c.display().to_string());
    }
    Err(format!(
        "no fake llama-server (looked for {}; build it with `cargo build -p nuggetd --features \
         fake-runtime --bin nuggetd-fake-llama-server`, or set {FAKE_SERVER_ENV})",
        tried.join(", ")
    ))
}

// --- the shell's files -----------------------------------------------------------------------

/// Environment variables that move the shell's per-user directories under `root`, so its
/// `app_config_dir` is [`app_config_dir`]`(root)`.
pub(crate) fn isolated_env(root: &Path) -> Vec<(&'static str, PathBuf)> {
    if cfg!(target_os = "macos") {
        // Tauri (via `dirs`) resolves `~/Library/Application Support` from `$HOME`.
        vec![("HOME", root.join("home"))]
    } else {
        vec![
            ("XDG_CONFIG_HOME", root.join("xdg-config")),
            ("XDG_DATA_HOME", root.join("xdg-data")),
            ("XDG_CACHE_HOME", root.join("xdg-cache")),
        ]
    }
}

/// The shell's config directory under the [`isolated_env`] of `root`.
pub(crate) fn app_config_dir(root: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        root.join("home/Library/Application Support").join(APP_ID)
    } else {
        root.join("xdg-config").join(APP_ID)
    }
}

/// `doubletap.json`: Double Tap on, read-ahead as given, also on battery; everything else at
/// the shell's defaults. (`read_ahead_on_battery` is unknown to shells before DD2; their
/// settings ignore unknown fields.)
pub(crate) fn doubletap_json(read_ahead: bool) -> String {
    let v = serde_json::json!({
        "enabled": true,
        "read_ahead": read_ahead,
        "read_ahead_on_battery": true,
    });
    format!("{v:#}\n")
}

/// `settings.json`: no update checks (no network), no start-at-login question or entry.
pub(crate) fn desktop_settings_json() -> String {
    let v = serde_json::json!({
        "channel": "off",
        "auto_install": false,
        "start_at_login": false,
        "autostart": false,
    });
    format!("{v:#}\n")
}

fn write_shell_config(root: &Path, read_ahead: bool) -> Result<(), BenchError> {
    let dir = app_config_dir(root);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("doubletap.json"), doubletap_json(read_ahead))?;
    std::fs::write(dir.join("settings.json"), desktop_settings_json())?;
    Ok(())
}

// --- the probe -------------------------------------------------------------------------------

/// Read-ahead activity in the shell's probe file over a window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProbeCounts {
    /// `draft.read_ahead` calls answered (`read_ahead` lines).
    pub(crate) sends: u64,
    /// ... that started a prefill (`Started { .. }`).
    pub(crate) started: u64,
    /// ... that the daemon skipped (`Skipped(..)`: rate limit, unchanged, refused, ...).
    pub(crate) skipped: u64,
    /// ... that failed (`Failed`: no daemon, error).
    pub(crate) failed: u64,
    /// `draft.read_ahead_cancel` calls (`read_ahead_cancel` lines).
    pub(crate) cancels: u64,
    /// `read_ahead_paused` lines (battery / low power).
    pub(crate) pauses: u64,
}

/// Count the probe lines with `from_ms <= t <= to_ms` (milliseconds since the Unix epoch, as
/// the probe writes them). Lines that do not parse are ignored.
pub(crate) fn count_probe(text: &str, from_ms: f64, to_ms: f64) -> ProbeCounts {
    let mut c = ProbeCounts::default();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(t) = v["t"].as_f64() else {
            continue;
        };
        if t < from_ms || t > to_ms {
            continue;
        }
        match v["event"].as_str() {
            Some("read_ahead") => {
                c.sends += 1;
                let d = v["detail"].as_str().unwrap_or_default();
                if d.starts_with("Started") {
                    c.started += 1;
                } else if d.starts_with("Skipped") {
                    c.skipped += 1;
                } else {
                    c.failed += 1;
                }
            }
            Some("read_ahead_cancel") => c.cancels += 1,
            Some("read_ahead_paused") => c.pauses += 1,
            _ => {}
        }
    }
    c
}

/// Whether the probe has an `event` line.
pub(crate) fn probe_has(text: &str, event: &str) -> bool {
    text.lines().any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .is_ok_and(|v| v["event"].as_str() == Some(event))
    })
}

fn epoch_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or_default()
}

// --- the focus script ------------------------------------------------------------------------

/// The scripted focus changes: `(seconds from the start, window index)`, one every `every_s`
/// from 0 while before `total_s`, cycling through `windows` in order.
pub(crate) fn focus_plan(total_s: f64, every_s: f64, windows: usize) -> Vec<(f64, usize)> {
    if windows == 0 || every_s <= 0.0 || total_s <= 0.0 {
        return Vec::new();
    }
    let n = (total_s / every_s - 1e-9).ceil().max(1.0) as usize;
    (0..n).map(|k| (k as f64 * every_s, k % windows)).collect()
}

/// Run `plan` from now until `total` has passed; returns (switches, failed switches, first
/// error).
fn run_plan(
    windows: &TextWindows,
    plan: &[(f64, usize)],
    total: Duration,
) -> (u64, u64, Option<String>) {
    let start = Instant::now();
    let (mut ok, mut failed, mut first) = (0, 0, None);
    for &(at, w) in plan {
        let due = start + Duration::from_secs_f64(at);
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
        match windows.activate(w) {
            Ok(()) => ok += 1,
            Err(e) => {
                failed += 1;
                first.get_or_insert(e);
            }
        }
    }
    std::thread::sleep((start + total).saturating_duration_since(Instant::now()));
    (ok, failed, first)
}

/// Run `cmd`, at most [`HELPER_LIMIT`]; its stdout, or why it failed.
fn run_bounded(cmd: &mut Command) -> Result<String, String> {
    let what = format!("{:?}", cmd.get_program());
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{what}: {e}"))?;
    let t = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                let mut err = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut out);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                return if status.success() {
                    Ok(out)
                } else {
                    Err(format!("{what} exited with {status}: {}", err.trim()))
                };
            }
            Ok(None) if t.elapsed() < HELPER_LIMIT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{what} did not finish within {HELPER_LIMIT:?}"));
            }
            Err(e) => return Err(format!("{what}: {e}")),
        }
    }
}

/// Three windows with a focused text field, and how to bring one to the front.
struct TextWindows {
    /// Linux: the window processes (closed on drop).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    procs: Vec<Child>,
    /// Linux: X window ids; macOS: document names.
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    ids: Vec<String>,
    /// What drives them, for the params.
    driver: &'static str,
}

impl Drop for TextWindows {
    fn drop(&mut self) {
        for c in &mut self.procs {
            let _ = c.kill();
            let _ = c.wait();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = run_bounded(
                Command::new("osascript")
                    .args(["-e", "tell application \"TextEdit\" to quit saving no"]),
            );
        }
    }
}

/// A Python that imports GTK 3 through PyGObject ([`PYTHON_ENV`] first).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn gtk_python() -> Result<String, String> {
    let env = std::env::var(PYTHON_ENV).ok().filter(|p| !p.is_empty());
    let candidates = env.into_iter().chain(
        ["python3", "/usr/bin/python3", "python3.12", "python3.11"]
            .into_iter()
            .map(str::to_owned),
    );
    for c in candidates {
        let ok = Command::new(&c)
            .args([
                "-c",
                "import gi; gi.require_version('Gtk', '3.0'); from gi.repository import Gtk",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Ok(c);
        }
    }
    Err(format!(
        "no Python with PyGObject and GTK 3 (install python3-gi gir1.2-gtk-3.0, or set \
         {PYTHON_ENV})"
    ))
}

/// Whether the focus driver's tools are here (checked before anything starts).
fn focus_tools() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if on_path("xdotool").is_none() {
            return Err("xdotool is not installed (needed to switch windows)".into());
        }
        gtk_python().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        if !Path::new("/usr/bin/osascript").is_file() && on_path("osascript").is_none() {
            return Err("osascript is missing (needed to switch TextEdit windows)".into());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("no focus driver on this platform".into())
    }
}

impl TextWindows {
    /// Open the [`WINDOWS`] (in `dir`) and wait until each is up.
    #[cfg(target_os = "linux")]
    fn open(dir: &Path) -> Result<Self, BenchError> {
        use std::sync::mpsc;

        let python = gtk_python().map_err(BenchError::Failed)?;
        let script = dir.join("energy-text-window.py");
        std::fs::write(&script, TEXT_WINDOW_PY)?;
        let mut me = Self {
            procs: Vec::new(),
            ids: Vec::new(),
            driver: "xdotool windowactivate --sync (GTK 3 windows, AT-SPI)",
        };
        for (title, text) in WINDOWS {
            let mut child = Command::new(&python)
                .arg(&script)
                .args([title, text])
                .env("GTK_MODULES", "gail:atk-bridge")
                .env("NO_AT_BRIDGE", "0")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| BenchError::Failed(format!("cannot start {python}: {e}")))?;
            let stdout = child.stdout.take();
            me.procs.push(child);
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                if let Some(s) = stdout {
                    for l in BufReader::new(s).lines().map_while(Result::ok) {
                        if l == "READY" {
                            let _ = tx.send(());
                        }
                    }
                }
            });
            rx.recv_timeout(HELPER_LIMIT).map_err(|_| {
                BenchError::Failed(format!("the text window `{title}` did not open"))
            })?;
            let out = run_bounded(Command::new("xdotool").args([
                "search",
                "--name",
                &format!("^{title}$"),
            ]))
            .map_err(BenchError::Failed)?;
            let id = out.lines().next().unwrap_or_default().trim().to_owned();
            if id.is_empty() {
                return Err(BenchError::Failed(format!("no X window titled `{title}`")));
            }
            me.ids.push(id);
        }
        Ok(me)
    }

    #[cfg(target_os = "macos")]
    fn open(dir: &Path) -> Result<Self, BenchError> {
        let mut files = Vec::new();
        let mut ids = Vec::new();
        for (i, (title, text)) in WINDOWS.iter().enumerate() {
            let stem = format!("nugget-bench-{} {title}", i + 1);
            let f = dir.join(format!("{stem}.txt"));
            std::fs::write(&f, text)?;
            files.push(f);
            ids.push(stem);
        }
        run_bounded(
            Command::new("/usr/bin/open")
                .args(["-a", "TextEdit"])
                .args(&files),
        )
        .map_err(BenchError::Failed)?;
        let me = Self {
            procs: Vec::new(),
            ids,
            driver: "osascript (TextEdit documents, Accessibility)",
        };
        let t = Instant::now();
        loop {
            let n = run_bounded(Command::new("osascript").args([
                "-e",
                "tell application \"TextEdit\" to count (windows whose name starts with \"nugget-bench-\")",
            ]))
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(0);
            if n >= WINDOWS.len() {
                return Ok(me);
            }
            if t.elapsed() > HELPER_LIMIT {
                return Err(BenchError::Failed(format!(
                    "TextEdit shows {n} of the {} documents",
                    WINDOWS.len()
                )));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn open(_dir: &Path) -> Result<Self, BenchError> {
        Err(BenchError::Failed(
            "no text windows on this platform".into(),
        ))
    }

    /// Bring window `i` to the front with its field focused.
    fn activate(&self, i: usize) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            run_bounded(Command::new("xdotool").args(["windowactivate", "--sync", &self.ids[i]]))
                .map(|_| ())
        }
        #[cfg(target_os = "macos")]
        {
            let script = format!(
                "tell application \"TextEdit\"\nactivate\nset index of (first window whose name \
                 starts with \"{}\") to 1\nend tell",
                self.ids[i]
            );
            run_bounded(Command::new("osascript").args(["-e", &script])).map(|_| ())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = i;
            Err("no focus driver on this platform".into())
        }
    }
}

// --- processes -------------------------------------------------------------------------------

/// Put `cmd` in a process group of its own, so a stop reaches its children (the webview).
fn own_group(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(cmd, 0);
    cmd
}

/// SIGTERM to the process group of `pid` (Unix), then wait for `child` at most `limit`, then
/// SIGKILL the group.
fn stop_group(child: &mut Child, limit: Duration) {
    let pid = child.id();
    #[cfg(unix)]
    let signal = |sig: &str| {
        let _ = Command::new("kill")
            .args(["-s", sig, "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    #[cfg(not(unix))]
    let signal = |_: &str| {
        let _ = pid;
    };
    signal("TERM");
    let t = Instant::now();
    while t.elapsed() < limit {
        if matches!(child.try_wait(), Ok(Some(_))) {
            // Children of the group that outlive the leader.
            signal("KILL");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    signal("KILL");
    let _ = child.kill();
    let _ = child.wait();
}

/// The last `n` lines of a log file.
fn tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The running shell. Stopped (with its process group) on drop.
struct Shell {
    child: Option<Child>,
    pid: u32,
    log: PathBuf,
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Shell {
    /// `exe --minimized` with `NUGGET_HOME`, the isolated directories of `root` and the probe.
    fn spawn(
        exe: &Path,
        env: &Env,
        root: &Path,
        probe: &Path,
        run: &str,
    ) -> Result<Self, BenchError> {
        // Its log (tracing writes to stdout) and stderr, in one file.
        let log = env.work.join(format!("shell-{run}.log"));
        let out = std::fs::File::create(&log)?;
        let mut cmd = Command::new(exe);
        cmd.arg("--minimized")
            .env("NUGGET_HOME", &env.home)
            .env("NUGGET_KEYCHAIN", "off")
            .env("NUGGET_E2E_PROBE", probe)
            .env_remove("NUGGET_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out);
        for (k, v) in isolated_env(root) {
            std::fs::create_dir_all(&v)?;
            cmd.env(k, v);
        }
        if cfg!(target_os = "linux") {
            // AT-SPI for GTK, as in apps/desktop/e2e-native.
            cmd.env("GTK_MODULES", "gail:atk-bridge")
                .env("NO_AT_BRIDGE", "0");
        }
        let child = own_group(&mut cmd)
            .spawn()
            .map_err(|e| BenchError::Failed(format!("cannot start {}: {e}", exe.display())))?;
        Ok(Self {
            pid: child.id(),
            child: Some(child),
            log,
        })
    }

    fn check_alive(&mut self) -> Result<(), BenchError> {
        let Some(c) = self.child.as_mut() else {
            return Err(BenchError::Failed("the shell already stopped".into()));
        };
        if let Some(status) = c.try_wait()? {
            return Err(BenchError::Failed(format!(
                "the shell exited early with {status}: {}",
                tail(&self.log, 20)
            )));
        }
        Ok(())
    }

    /// Wait until it is set up: the probe's `read_ahead_loop` line then `settle`, or (no probe
    /// line within [`PROBE_WAIT`], a release build) [`PROBE_WAIT`] alone. Returns whether the
    /// probe answered.
    fn settle(&mut self, probe: &Path, settle: Duration) -> Result<bool, BenchError> {
        let t = Instant::now();
        while t.elapsed() < PROBE_WAIT {
            self.check_alive()?;
            let text = std::fs::read_to_string(probe).unwrap_or_default();
            if probe_has(&text, "read_ahead_loop") {
                std::thread::sleep(settle);
                self.check_alive()?;
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        self.check_alive()?;
        Ok(false)
    }

    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            stop_group(&mut c, Duration::from_secs(10));
        }
    }
}

/// The fake `llama-server` on a loopback port. Killed on drop.
struct FakeServer {
    child: Child,
    pid: u32,
    port: u16,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        stop_group(&mut self.child, Duration::from_secs(5));
    }
}

/// A port nothing listens on right now.
fn free_port() -> Result<u16, BenchError> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port())
}

/// The status code of `GET path` on the loopback `port`, if it answers.
fn http_status(port: u16, path: &str) -> Option<u16> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    line.split_whitespace().nth(1)?.parse().ok()
}

impl FakeServer {
    fn start(exe: &Path, work: &Path, timeout: Duration) -> Result<Self, BenchError> {
        let model = work.join("bench-local.gguf");
        std::fs::write(&model, b"GGUF")?;
        let port = free_port()?;
        let mut cmd = Command::new(exe);
        cmd.args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["--alias", PROVIDER, "--model"])
            .arg(&model)
            .env_remove("LLAMA_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(work.join("fake-server.stderr"))?);
        let child = own_group(&mut cmd)
            .spawn()
            .map_err(|e| BenchError::Failed(format!("cannot start {}: {e}", exe.display())))?;
        let mut me = Self {
            pid: child.id(),
            child,
            port,
        };
        let t = Instant::now();
        while http_status(port, "/health") != Some(200) {
            if let Some(status) = me.child.try_wait()? {
                return Err(BenchError::Failed(format!(
                    "the fake llama-server exited with {status}: {}",
                    tail(&work.join("fake-server.stderr"), 10)
                )));
            }
            if t.elapsed() > timeout {
                return Err(BenchError::Failed(
                    "the fake llama-server never answered /health".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(me)
    }
}

/// The daemon's config with the fake server as its only provider (on-device: loopback).
pub(crate) fn provider_block(port: u16) -> String {
    format!(
        "\n[[providers]]\nid = \"{PROVIDER}\"\nkind = \"openai_compatible\"\n\
         base_url = \"http://127.0.0.1:{port}/v1\"\nmodel = \"{PROVIDER}\"\n\
         tiers = [\"triage\", \"light\", \"synthesis\"]\n"
    )
}

/// A daemon on an empty temporary home (`perf_daemon`'s seeding and hermetic config), plus
/// `extra` config; ready, with no client left connected.
fn start_daemon(env: &Env, seed: u64, extra: &str, run: &str) -> Result<Daemon, BenchError> {
    let empty = Corpus::generate(CorpusConfig {
        docs: 0,
        seed,
        ..CorpusConfig::default()
    });
    seed_home(&env.paths, &empty, DIM, seed)?;
    write_config(&env.paths, None)?;
    if !extra.is_empty() {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(env.paths.config_file())?;
        f.write_all(extra.as_bytes())?;
    }
    runtime()?.block_on(async {
        let mut d = Daemon::spawn(env, run)?;
        let (_, client) = d.ready(env, Instant::now()).await?;
        drop(client);
        Ok(d)
    })
}

fn stop_daemon(mut d: Daemon) -> Result<(), BenchError> {
    runtime()?.block_on(d.stop())?;
    Ok(())
}

// --- metrics ---------------------------------------------------------------------------------

/// A process group's figures: [`window_metrics`] plus, on macOS, `energy_impact`.
fn process_metrics(prefix: &str, what: &str, r: &Reading) -> Vec<Metric> {
    let mut out = window_metrics(prefix, what, r);
    if let Some(e) = r.energy_impact {
        out.push(Metric::new(
            format!("{prefix}.energy_impact"),
            e,
            "score/s",
            Direction::LowerIsBetter,
            format!("macOS energy impact of {what} per second (powermetrics tasks, unitless)"),
        ));
    }
    out
}

/// The system's figures over a window (package power; with a baseline, attributable).
fn system_metrics(prefix: &str, what: &str, r: &Reading, base: Option<&Reading>) -> Vec<Metric> {
    let sys = Reading {
        energy_impact: None,
        ..r.clone()
    };
    energy_metrics(prefix, what, &sys, base)
}

fn count_metric(name: String, v: u64, what: &str) -> Metric {
    Metric::new(name, v as f64, "count", Direction::Info, what)
}

/// The probe counts of a window as metrics under `prefix`.
fn probe_metrics(prefix: &str, c: &ProbeCounts) -> Vec<Metric> {
    vec![
        count_metric(
            format!("{prefix}.read_ahead_sends"),
            c.sends,
            "draft.read_ahead calls the shell made in the window (probe)",
        ),
        count_metric(
            format!("{prefix}.read_ahead_started"),
            c.started,
            "read-aheads the daemon started (probe: Started)",
        ),
        count_metric(
            format!("{prefix}.read_ahead_skipped"),
            c.skipped,
            "read-aheads the daemon skipped: rate limit, unchanged prompt, refused (probe)",
        ),
        count_metric(
            format!("{prefix}.read_ahead_failed"),
            c.failed,
            "read-ahead calls that failed (probe)",
        ),
        count_metric(
            format!("{prefix}.read_ahead_cancels"),
            c.cancels,
            "draft.read_ahead_cancel calls (focus left a field while a read-ahead could run; probe)",
        ),
    ]
}

/// Record a group's sources under `<key>.*` (as [`record`]).
fn record_group(ctx: &mut RunContext, key: &str, r: &Reading) {
    ctx.param(&format!("{key}.process_source"), r.process_source);
    ctx.param(&format!("{key}.processes"), r.processes);
}

// --- the scenarios ---------------------------------------------------------------------------

pub(super) fn shell_idle(
    ctx: &mut RunContext,
    p: &EnergyParams,
    source: EnergySource,
) -> Result<Vec<Metric>, BenchError> {
    let s = Scenario::ShellIdle;
    let name = s.as_str();
    let found = match preconditions() {
        Ok(f) => f,
        Err(why) => {
            skip(ctx, s, &why);
            return Ok(Vec::new());
        }
    };
    ctx.param(name, "measured");
    ctx.param(
        &format!("{name}.shell_exe"),
        found.shell.display().to_string(),
    );
    ctx.param(
        &format!("{name}.stack"),
        "nugget-desktop --minimized (hidden in the tray; Double Tap on, read-ahead on, update \
         channel off, isolated config dir) connected to nuggetd on an empty temporary \
         NUGGET_HOME (no providers, [runtime], [mcp] and vault off); nothing in front but the \
         desktop; shell metered with its descendants (webview processes), daemon separately",
    );
    ctx.param(&format!("{name}.idle_s"), p.idle_s);

    let window = Duration::from_secs_f64(p.idle_s);
    let base = wants_baseline(source).then(|| baseline(window));
    let tmp = tempfile::Builder::new()
        .prefix("nugget-bench-energy-shell-")
        .tempdir()?;
    let env = env_for(&found.daemon, tmp.path(), p.timeout_s);
    let d = start_daemon(&env, ctx.seed(), "", "shell-idle")?;
    let mut d = d;
    let root = tmp.path().join("shell");
    write_shell_config(&root, true)?;
    let probe = tmp.path().join("probe.jsonl");
    let mut shell = Shell::spawn(&found.shell, &env, &root, &probe, "idle")?;
    let probed = shell.settle(&probe, Duration::from_secs_f64(p.shell_settle_s))?;
    d.check_alive()?;
    let from = epoch_ms();
    let meter = Meter::start_groups(&[&[shell.pid], &[d.pid]]);
    std::thread::sleep(window);
    let mut rs = meter.stop_groups();
    let to = epoch_ms();
    shell.check_alive()?;
    d.check_alive()?;
    shell.stop();
    stop_daemon(d)?;
    let rd = rs.pop().unwrap_or_default();
    let rsh = rs.pop().unwrap_or_default();
    let counts = probed.then(|| {
        count_probe(
            &std::fs::read_to_string(&probe).unwrap_or_default(),
            from,
            to,
        )
    });
    drop(tmp);

    record(ctx, name, &rsh, base.as_ref());
    record_group(ctx, &format!("{name}.shell"), &rsh);
    record_group(ctx, &format!("{name}.daemon"), &rd);
    ctx.param(&format!("{name}.probe"), probe_note(probed));
    let prefix = format!("energy.{name}");
    let mut out = process_metrics(&format!("{prefix}.shell"), "the idle shell", &rsh);
    out.extend(process_metrics(
        &format!("{prefix}.daemon"),
        "the daemon beside the idle shell",
        &rd,
    ));
    out.extend(system_metrics(
        &prefix,
        "the idle shell and daemon",
        &rsh,
        base.as_ref(),
    ));
    if let Some(c) = &counts {
        out.extend(probe_metrics(&prefix, c));
    }
    Ok(out)
}

fn probe_note(probed: bool) -> &'static str {
    if probed {
        "debug build: settled on the probe's read_ahead_loop line; read-ahead counts reported"
    } else {
        "no probe lines (release build?): fixed settle, no read-ahead counts"
    }
}

/// One `shell_focus` run's readings.
struct Arm {
    shell: Reading,
    daemon: Reading,
    server: Reading,
    switches: u64,
    switch_errors: u64,
    first_error: Option<String>,
    counts: Option<ProbeCounts>,
}

pub(super) fn shell_focus(
    ctx: &mut RunContext,
    p: &EnergyParams,
    source: EnergySource,
) -> Result<Vec<Metric>, BenchError> {
    let s = Scenario::ShellFocus;
    let name = s.as_str();
    let found = match preconditions() {
        Ok(f) => f,
        Err(why) => {
            skip(ctx, s, &why);
            return Ok(Vec::new());
        }
    };
    let server_exe = match focus_tools().and_then(|()| find_fake_server(&found.daemon)) {
        Ok(f) => f,
        Err(why) => {
            skip(ctx, s, &why);
            return Ok(Vec::new());
        }
    };
    let plan = focus_plan(p.focus_s, p.focus_every_s, WINDOWS.len());
    let total = Duration::from_secs_f64(p.focus_s);
    ctx.param(name, "measured");
    ctx.param(
        &format!("{name}.shell_exe"),
        found.shell.display().to_string(),
    );
    ctx.param(
        &format!("{name}.server_exe"),
        server_exe.display().to_string(),
    );
    ctx.param(
        &format!("{name}.stack"),
        "nugget-desktop --minimized (Double Tap on, read-ahead off then on, update channel off, \
         isolated config dir) connected to nuggetd on an empty temporary NUGGET_HOME whose only \
         provider is the fake llama-server on a loopback port (openai_compatible, on-device: \
         read-ahead prefills with cache_prompt; the fake answers at once); a scripted window of \
         focus switches among three text-field windows; shell (with descendants), daemon and \
         server metered separately; the shell restarted between the runs",
    );
    ctx.param(&format!("{name}.focus_s"), p.focus_s);
    ctx.param(&format!("{name}.focus_every_s"), p.focus_every_s);
    ctx.param(
        &format!("{name}.windows"),
        WINDOWS.iter().map(|w| w.0).collect::<Vec<_>>(),
    );
    ctx.param(
        &format!("{name}.plan"),
        plan.iter()
            .map(|(t, w)| format!("{t}s:{}", WINDOWS[*w].0))
            .collect::<Vec<_>>(),
    );

    let tmp = tempfile::Builder::new()
        .prefix("nugget-bench-energy-shell-")
        .tempdir()?;
    let windows = TextWindows::open(tmp.path())?;
    ctx.param(&format!("{name}.driver"), windows.driver);
    // The baseline: the same script with nothing of Nugget running.
    let base = wants_baseline(source).then(|| {
        let m = Meter::start(&[]);
        let _ = run_plan(&windows, &plan, total);
        m.stop()
    });
    let env = env_for(&found.daemon, tmp.path(), p.timeout_s);
    let server = FakeServer::start(&server_exe, tmp.path(), env.timeout)?;
    let mut d = start_daemon(
        &env,
        ctx.seed(),
        &provider_block(server.port),
        "shell-focus",
    )?;
    let settle = Duration::from_secs_f64(p.shell_settle_s);
    let mut arms = Vec::new();
    let mut probed_all = true;
    for read_ahead in [false, true] {
        let run = if read_ahead { "on" } else { "off" };
        eprintln!("energy: {name}, read-ahead {run}");
        let root = tmp.path().join(format!("shell-{run}"));
        write_shell_config(&root, read_ahead)?;
        let probe = tmp.path().join(format!("probe-{run}.jsonl"));
        let mut shell = Shell::spawn(&found.shell, &env, &root, &probe, run)?;
        let probed = shell.settle(&probe, settle)?;
        probed_all &= probed;
        d.check_alive()?;
        let from = epoch_ms();
        let meter = Meter::start_groups(&[&[shell.pid], &[d.pid], &[server.pid]]);
        let (switches, switch_errors, first_error) = run_plan(&windows, &plan, total);
        let mut rs = meter.stop_groups();
        let to = epoch_ms();
        shell.check_alive()?;
        d.check_alive()?;
        shell.stop();
        let server_r = rs.pop().unwrap_or_default();
        let daemon_r = rs.pop().unwrap_or_default();
        let shell_r = rs.pop().unwrap_or_default();
        let counts = probed.then(|| {
            count_probe(
                &std::fs::read_to_string(&probe).unwrap_or_default(),
                from,
                to,
            )
        });
        arms.push((
            run,
            Arm {
                shell: shell_r,
                daemon: daemon_r,
                server: server_r,
                switches,
                switch_errors,
                first_error,
                counts,
            },
        ));
        // Let the daemon and server finish what the run left (an unanswered prefill).
        std::thread::sleep(Duration::from_secs_f64(p.settle_s));
    }
    stop_daemon(d)?;
    drop(server);
    drop(windows);
    drop(tmp);

    ctx.param(&format!("{name}.probe"), probe_note(probed_all));
    let mut out = Vec::new();
    let mut totals = Vec::new();
    for (run, a) in &arms {
        let key = format!("{name}.read_ahead_{run}");
        record(ctx, &key, &a.shell, base.as_ref());
        record_group(ctx, &format!("{key}.shell"), &a.shell);
        record_group(ctx, &format!("{key}.daemon"), &a.daemon);
        record_group(ctx, &format!("{key}.server"), &a.server);
        if let Some(e) = &a.first_error {
            ctx.param(&format!("{key}.switch_error"), e.clone());
        }
        let prefix = format!("energy.{key}");
        let what = format!("read-ahead {run}");
        out.extend(process_metrics(
            &format!("{prefix}.shell"),
            &format!("the shell during focus changes ({what})"),
            &a.shell,
        ));
        out.extend(process_metrics(
            &format!("{prefix}.daemon"),
            &format!("the daemon during focus changes ({what})"),
            &a.daemon,
        ));
        out.extend(process_metrics(
            &format!("{prefix}.server"),
            &format!("the fake local model server during focus changes ({what})"),
            &a.server,
        ));
        out.extend(system_metrics(
            &prefix,
            &format!("the focus script ({what})"),
            &a.shell,
            base.as_ref(),
        ));
        out.push(count_metric(
            format!("{prefix}.switches"),
            a.switches,
            "Focus switches made in the window",
        ));
        out.push(count_metric(
            format!("{prefix}.switch_errors"),
            a.switch_errors,
            "Focus switches that failed (params: switch_error)",
        ));
        if let Some(c) = &a.counts {
            out.extend(probe_metrics(&prefix, c));
        }
        let sum = |f: fn(&Reading) -> Option<f64>| -> Option<f64> {
            Some(f(&a.shell)? + f(&a.daemon)? + f(&a.server)?)
        };
        totals.push((sum(Reading::cpu_percent), sum(Reading::wakeups_per_s)));
    }
    if let [(off_cpu, off_wake), (on_cpu, on_wake)] = totals[..] {
        let added = format!("energy.{name}.read_ahead_added");
        if let (Some(on), Some(off)) = (on_cpu, off_cpu) {
            out.push(Metric::new(
                format!("{added}.cpu_percent"),
                on - off,
                "%",
                Direction::LowerIsBetter,
                "CPU of shell + daemon + server with read-ahead on minus off, share of one core",
            ));
        }
        if let (Some(on), Some(off)) = (on_wake, off_wake) {
            out.push(Metric::new(
                format!("{added}.wakeups_per_s"),
                on - off,
                "1/s",
                Direction::LowerIsBetter,
                "Wakeups of shell + daemon + server with read-ahead on minus off",
            ));
        }
    }
    ctx.param(&format!("{name}.window_s"), round3(p.focus_s));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_switches_every_few_seconds_in_order() {
        let plan = focus_plan(60.0, 5.0, 3);
        assert_eq!(plan.len(), 12);
        assert_eq!(plan[0], (0.0, 0));
        assert_eq!(plan[1], (5.0, 1));
        assert_eq!(plan[3], (15.0, 0));
        assert_eq!(plan[11], (55.0, 2));
        assert_eq!(focus_plan(15.0, 5.0, 3).len(), 3);
        assert_eq!(focus_plan(16.0, 5.0, 3).len(), 4);
        assert_eq!(focus_plan(2.0, 5.0, 3), [(0.0, 0)]);
        assert!(focus_plan(60.0, 5.0, 0).is_empty());
        assert!(focus_plan(0.0, 5.0, 3).is_empty());
        assert_eq!(focus_plan(60.0, 5.0, 3), focus_plan(60.0, 5.0, 3));
    }

    #[test]
    fn probe_lines_are_counted_inside_the_window() {
        let text = [
            r#"{"t":1000.0,"event":"read_ahead_loop","detail":{"running":true}}"#,
            r#"{"t":1500.0,"event":"read_ahead","detail":"Started { done: true }"}"#,
            r#"{"t":2000.0,"event":"read_ahead","detail":"Started { done: true }"}"#,
            r#"{"t":2100.0,"event":"read_ahead","detail":"Skipped(RateLimited)"}"#,
            r#"{"t":2200.0,"event":"read_ahead","detail":"Failed"}"#,
            r#"{"t":2300.0,"event":"read_ahead_cancel","detail":null}"#,
            r#"{"t":2400.0,"event":"read_ahead_paused","detail":{"paused":true}}"#,
            "not json",
            r#"{"t":9000.0,"event":"read_ahead","detail":"Started { done: false }"}"#,
        ]
        .join("\n");
        let c = count_probe(&text, 1800.0, 3000.0);
        assert_eq!(
            c,
            ProbeCounts {
                sends: 3,
                started: 1,
                skipped: 1,
                failed: 1,
                cancels: 1,
                pauses: 1,
            }
        );
        assert_eq!(count_probe(&text, 0.0, f64::MAX).sends, 5);
        assert!(probe_has(&text, "read_ahead_loop"));
        assert!(!probe_has(&text, "trigger"));
        assert_eq!(count_probe("", 0.0, 1.0), ProbeCounts::default());
    }

    #[test]
    fn settings_files() {
        for on in [true, false] {
            let v: serde_json::Value = serde_json::from_str(&doubletap_json(on)).unwrap();
            assert_eq!(v["enabled"], true);
            assert_eq!(v["read_ahead"], on);
            assert_eq!(v["read_ahead_on_battery"], true);
        }
        let v: serde_json::Value = serde_json::from_str(&desktop_settings_json()).unwrap();
        assert_eq!(v["channel"], "off");
        assert_eq!(v["autostart"], false);

        let dir = tempfile::tempdir().unwrap();
        write_shell_config(dir.path(), false).unwrap();
        let cfg = app_config_dir(dir.path());
        assert!(cfg.ends_with(APP_ID));
        let dt = std::fs::read_to_string(cfg.join("doubletap.json")).unwrap();
        assert!(dt.contains("\"read_ahead\": false"));
        assert!(cfg.join("settings.json").is_file());
        // The config dir sits where the moved directory makes Tauri look.
        let env = isolated_env(dir.path());
        let (_, base) = &env[0];
        assert!(cfg.starts_with(base));
        if cfg!(target_os = "macos") {
            assert_eq!(env[0].0, "HOME");
        } else {
            assert_eq!(env[0].0, "XDG_CONFIG_HOME");
            assert_eq!(cfg.parent().unwrap(), base);
        }
    }

    #[test]
    fn provider_is_on_device() {
        let b = provider_block(4321);
        assert!(b.contains("base_url = \"http://127.0.0.1:4321/v1\""));
        assert!(b.contains("kind = \"openai_compatible\""));
        assert!(b.contains(&format!("id = \"{PROVIDER}\"")));
        assert!(b.starts_with("\n[[providers]]\n"));
    }

    #[test]
    fn text_window_script_is_embedded() {
        assert!(TEXT_WINDOW_PY.contains("READY"));
        assert_eq!(WINDOWS.len(), 3);
        let titles: std::collections::HashSet<_> = WINDOWS.iter().map(|w| w.0).collect();
        assert_eq!(
            titles.len(),
            3,
            "distinct titles: read-ahead keys on the window"
        );
    }

    #[test]
    fn a_missing_shell_is_a_skip_reason() {
        // Only checkable where the variable is unset (CI's unit tests).
        if std::env::var_os(SHELL_ENV).is_none()
            && cfg!(any(target_os = "linux", target_os = "macos"))
        {
            assert!(
                preconditions()
                    .err()
                    .unwrap_or_default()
                    .contains(SHELL_ENV)
            );
        }
    }
}
