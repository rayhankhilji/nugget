//! The accessibility app matrix: real browsers on the web fixtures (`tests/fixtures/apps/web`,
//! contract in `PROTOCOL.md`) and native editors, measured through the public `nugget-a11y` API.
//!
//! One JSON record per cell (app × fixture editor, or one per native editor) goes to the JSON
//! Lines file in `NUGGET_A11Y_MATRIX_OUT`; `render_matrix.py` turns those into Markdown. The run
//! fails only when a metric listed in `expected.json` fails (see [`measure::metric_passes`]) or
//! when the harness itself cannot start.
//!
//! Environment:
//! - `NUGGET_A11Y_APPS`: comma list of app ids (default: every app of this OS that is installed);
//! - `NUGGET_A11Y_APPS_EDITORS`: comma list of fixture editor ids (default: all in `truth.json`);
//! - `NUGGET_A11Y_MATRIX_OUT`: the JSONL file (default `<CARGO_TARGET_TMPDIR>/apps-matrix/
//!   results.jsonl`); screenshots of failing cells go to `screens/` next to it;
//! - `NUGGET_A11Y_APPS_FIXTURES`: the fixtures dir (default `tests/fixtures/apps`);
//! - `NUGGET_A11Y_APPS_FORCE_A11Y=1`: start Chromium with `--force-renderer-accessibility`
//!   (a diagnostic variant; recorded per cell);
//! - `NUGGET_A11Y_APPS_EXE_<ID>`: the executable (or `.app`) of one app.

#![allow(dead_code)]

pub mod launch;
pub mod measure;
pub mod server;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nugget_a11y::computer::KeyChord;
use nugget_a11y::{A11yConfig, Accessibility, ComputerUse, Focused, KeyInput, Permission};
use serde_json::{Value, json};

use launch::{AppSpec, Kind, Native, Running};
use measure::{GotMessage, TruthMessage, norm, trunc};
use server::Server;

/// The text native editors start from (and the seed file holds).
pub const SEED: &str = "Draft: ";
/// The fixture page title prefix (PROTOCOL.md).
pub const TITLE: &str = "Nugget fixture";

pub struct Config {
    pub apps: Option<Vec<String>>,
    pub editors: Option<Vec<String>>,
    pub out: PathBuf,
    pub fixtures: PathBuf,
    pub force_a11y: bool,
}

fn list_env(var: &str) -> Option<Vec<String>> {
    let v = std::env::var(var).ok()?;
    let items: Vec<String> = v
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (!items.is_empty()).then_some(items)
}

impl Config {
    pub fn from_env() -> Config {
        let fixtures = std::env::var_os("NUGGET_A11Y_APPS_FIXTURES")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests")
                    .join("fixtures")
                    .join("apps")
            });
        let out = std::env::var_os("NUGGET_A11Y_MATRIX_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_TARGET_TMPDIR"))
                    .join("apps-matrix")
                    .join("results.jsonl")
            });
        Config {
            apps: list_env("NUGGET_A11Y_APPS"),
            editors: list_env("NUGGET_A11Y_APPS_EDITORS"),
            out,
            fixtures,
            force_a11y: std::env::var("NUGGET_A11Y_APPS_FORCE_A11Y").is_ok_and(|v| v == "1"),
        }
    }

    fn out_dir(&self) -> PathBuf {
        self.out
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// One fixture editor from `truth.json`.
#[derive(Debug, Clone)]
pub struct Editor {
    pub id: String,
    pub name: String,
    pub context: String,
    pub kind: String,
}

pub struct Truth {
    pub initial_text: String,
    pub editors: Vec<Editor>,
    pub raw: Value,
}

impl Truth {
    pub fn load(path: &Path) -> Result<Truth, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Truth::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Truth, String> {
        let raw: Value = serde_json::from_str(text).map_err(|e| format!("truth.json: {e}"))?;
        let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let editors: Vec<Editor> = raw["editors"]
            .as_array()
            .ok_or("truth.json: no editors")?
            .iter()
            .map(|e| Editor {
                id: s(&e["id"]),
                name: s(&e["name"]),
                context: s(&e["context"]),
                kind: s(&e["kind"]),
            })
            .filter(|e| !e.id.is_empty())
            .collect();
        if editors.is_empty() {
            return Err("truth.json: no editors".into());
        }
        Ok(Truth {
            initial_text: raw["initial_text"].as_str().unwrap_or(SEED).to_string(),
            editors,
            raw,
        })
    }

    pub fn messages(&self, context: &str) -> Vec<TruthMessage> {
        self.raw["contexts"][context]["messages"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|m| TruthMessage {
                        text: m["text"].as_str().unwrap_or_default().to_string(),
                        from_me: m["from_me"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn recipients(&self, context: &str) -> Vec<String> {
        self.raw["contexts"][context]["recipients"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| r.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn subject(&self, context: &str) -> Option<String> {
        self.raw["contexts"][context]["subject"]
            .as_str()
            .map(str::to_string)
    }
}

/// The JSON Lines output, flushed after every record (partial runs keep their results).
pub struct Out {
    file: std::fs::File,
    pub records: Vec<Value>,
}

impl Out {
    pub fn create(path: &Path) -> std::io::Result<Out> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        Ok(Out {
            file: std::fs::File::create(path)?,
            records: Vec::new(),
        })
    }

    pub fn write(&mut self, rec: Value) {
        let line = rec.to_string();
        let _ = writeln!(self.file, "{line}");
        let _ = self.file.flush();
        eprintln!("CELL {}", summary(&rec));
        self.records.push(rec);
    }
}

/// A one-line summary of a cell for the log.
pub fn summary(rec: &Value) -> String {
    let inserts = rec["inserts"].as_array().cloned().unwrap_or_default();
    let ok = inserts.iter().filter(|i| i["exact"] == true).count();
    format!(
        "{} available={} detect={} read={} secure={} thread=P{}/R{} inserts={}/{} undo={}/{} \
         read_ms={} error={}",
        measure::cell_key(rec),
        rec["available"],
        rec["detect"],
        rec["read_ok"],
        rec["secure_ok"],
        rec["thread"]["precision"],
        rec["thread"]["recall"],
        ok,
        inserts.len(),
        inserts
            .iter()
            .filter(|i| i["undo_one_step"] == true)
            .count(),
        inserts.len(),
        rec["read_ms"]["median"],
        rec["error"]
    )
}

/// The shared parts of every record of this run.
struct Env {
    os: &'static str,
    os_version: String,
    /// The CI runner label (`NUGGET_A11Y_RUNNER`, e.g. `windows-2022`), when set.
    runner: Option<String>,
    force_a11y: bool,
}

impl Env {
    fn record(&self, app: &AppSpec, version: Option<&str>, fixture: &str) -> Value {
        json!({
            "os": self.os,
            "os_version": self.os_version,
            "runner": self.runner,
            "app": app.id,
            "app_version": version,
            "fixture": fixture,
            "kind": null,
            "context": null,
            "available": true,
            "detect": null,
            "role": null,
            "label": null,
            "read_ok": null,
            "thread": null,
            "secure_ok": null,
            "inserts": [],
            "read_ms": null,
            "oracle": null,
            "force_a11y": self.force_a11y && app.is_chromium(),
        })
    }
}

pub fn os_id() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

fn undo_chord() -> KeyInput {
    keys(if cfg!(target_os = "macos") {
        "cmd+z"
    } else {
        "ctrl+z"
    })
}

fn end_of_document() -> KeyInput {
    keys(if cfg!(target_os = "macos") {
        "cmd+down"
    } else {
        "ctrl+end"
    })
}

fn select_all() -> KeyInput {
    keys(if cfg!(target_os = "macos") {
        "cmd+a"
    } else {
        "ctrl+a"
    })
}

fn keys(s: &str) -> KeyInput {
    KeyInput::Keys(KeyChord::parse_list(s).expect("valid chord"))
}

fn last_text(s: &Option<Value>) -> String {
    s.as_ref()
        .and_then(|s| s["text"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// Run the whole matrix on this OS. Panics only when the harness cannot start or an expected
/// metric fails.
pub fn run_matrix() {
    let cfg = Config::from_env();
    let expected_path = cfg.fixtures.join("expected.json");
    let expected: Value = match std::fs::read_to_string(&expected_path) {
        Ok(t) => {
            serde_json::from_str(&t).unwrap_or_else(|e| panic!("{}: {e}", expected_path.display()))
        }
        Err(_) => json!({}),
    };
    let mut out = Out::create(&cfg.out)
        .unwrap_or_else(|e| panic!("cannot create {}: {e}", cfg.out.display()));
    let env = Env {
        os: os_id(),
        os_version: launch::os_version(),
        runner: std::env::var("NUGGET_A11Y_RUNNER")
            .ok()
            .filter(|r| !r.is_empty()),
        force_a11y: cfg.force_a11y,
    };
    eprintln!(
        "apps matrix on {} ({}); results in {}",
        env.os,
        env.os_version,
        cfg.out.display()
    );

    let a11y = nugget_a11y::system_with(A11yConfig::default());
    let cu = nugget_a11y::system_computer_with(A11yConfig::default());
    let env_error = permission(&*a11y);

    let all = launch::all_apps();
    if let Some(want) = &cfg.apps {
        for w in want {
            if !all.iter().any(|a| a.id == w) {
                eprintln!("note: app {w:?} is not part of the matrix on {}", env.os);
            }
        }
    }
    let specs: Vec<AppSpec> = all
        .into_iter()
        .filter(|a| {
            cfg.apps
                .as_ref()
                .is_none_or(|w| w.iter().any(|x| x == a.id))
        })
        .collect();

    // The fixture server, when a browser is in the run.
    let browsers = specs.iter().any(|s| matches!(s.kind, Kind::Browser(_)));
    let web = if browsers {
        let truth = Truth::load(&cfg.fixtures.join("truth.json"))
            .unwrap_or_else(|e| panic!("harness cannot start: {e}"));
        assert!(
            cfg.fixtures.join("web").join("index.html").exists(),
            "harness cannot start: no web/index.html in {}",
            cfg.fixtures.display()
        );
        let srv = Server::start(&cfg.fixtures)
            .unwrap_or_else(|e| panic!("harness cannot start the fixture server: {e}"));
        eprintln!("fixture server on {}", srv.base_url());
        Some((srv, truth))
    } else {
        None
    };

    // Profiles and documents outside the uploaded output dir (browsers can keep files locked);
    // app logs inside it.
    let work = std::env::temp_dir().join(format!("nugget-apps-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&work);
    launch::set_log_dir(cfg.out_dir().join("logs"));
    let screens = cfg.out_dir().join("screens");

    for spec in &specs {
        let exe = match launch::locate(spec) {
            Ok(p) => p,
            Err(why) => {
                let mut rec = env.record(spec, None, "*");
                rec["available"] = json!(false);
                rec["error"] = json!(why);
                out.write(rec);
                continue;
            }
        };
        let version = launch::version(spec, &exe);
        eprintln!("== {} {:?} at {}", spec.id, version, exe.display());
        let app_dir = work.join(spec.id);
        let _ = std::fs::create_dir_all(&app_dir);
        match spec.kind {
            Kind::Browser(_) => {
                let (srv, truth) = web.as_ref().expect("server started for browsers");
                for ed in &truth.editors {
                    if cfg.editors.as_ref().is_some_and(|w| !w.contains(&ed.id)) {
                        continue;
                    }
                    let mut rec = env.record(spec, version.as_deref(), &ed.id);
                    rec["kind"] = json!(ed.kind);
                    rec["context"] = json!(ed.context);
                    rec["oracle"] = json!("page");
                    if let Some(e) = &env_error {
                        rec["error"] = json!(e);
                        out.write(rec);
                        continue;
                    }
                    let mut ctx = Cell {
                        a11y: &*a11y,
                        cu: &*cu,
                        rec: &mut rec,
                        running: None,
                    };
                    let r = guarded(|| {
                        web_cell(
                            &mut ctx,
                            srv,
                            truth,
                            ed,
                            spec,
                            &exe,
                            &app_dir,
                            cfg.force_a11y,
                        )
                    });
                    finish(&mut ctx, r, &screens);
                    drop(ctx);
                    out.write(rec);
                }
            }
            Kind::Native(n) => {
                let mut rec = env.record(spec, version.as_deref(), "native");
                rec["kind"] = json!("native");
                rec["oracle"] = json!(if n == Native::TextEdit {
                    "osascript"
                } else {
                    "a11y"
                });
                if let Some(e) = &env_error {
                    rec["error"] = json!(e);
                    out.write(rec);
                    continue;
                }
                let mut ctx = Cell {
                    a11y: &*a11y,
                    cu: &*cu,
                    rec: &mut rec,
                    running: None,
                };
                let r = guarded(|| native_cell(&mut ctx, spec, n, &exe, &app_dir, &cfg.fixtures));
                finish(&mut ctx, r, &screens);
                drop(ctx);
                out.write(rec);
            }
        }
        let _ = std::fs::remove_dir_all(&app_dir);
    }
    let _ = std::fs::remove_dir_all(&work);
    drop(web);

    let failures = measure::gate(&expected, &out.records);
    eprintln!(
        "apps matrix: {} cells written to {}",
        out.records.len(),
        cfg.out.display()
    );
    assert!(
        failures.is_empty(),
        "expected metrics failed ({}):\n{}",
        expected_path.display(),
        failures.join("\n")
    );
}

/// `Some(reason)` when the accessibility API cannot be used at all on this machine.
fn permission(a11y: &dyn Accessibility) -> Option<String> {
    let p = a11y.permission();
    if cfg!(target_os = "linux") && p != Permission::Granted {
        // Enabling AT-SPI is what Nugget does on Linux; apps started afterwards see it.
        if let Err(e) = a11y.request_permission() {
            return Some(format!("cannot enable AT-SPI: {e}"));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while a11y.permission() != Permission::Granted {
            if Instant::now() > deadline {
                return Some("AT-SPI stays disabled after request_permission()".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        return None;
    }
    if p == Permission::Denied {
        let why = format!(
            "accessibility permission not granted to {} (macOS: AXIsProcessTrusted is false)",
            std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
        println!("::warning title=nugget-a11y apps matrix::{why}");
        return Some(why);
    }
    None
}

/// Run `f`, turning a panic into an error.
fn guarded(f: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => Err(format!(
            "panic: {}",
            p.downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default()
        )),
    }
}

/// One cell in progress.
struct Cell<'a> {
    a11y: &'a dyn Accessibility,
    cu: &'a dyn ComputerUse,
    rec: &'a mut Value,
    running: Option<Running>,
}

fn cell_failed(rec: &Value) -> bool {
    rec.get("error").is_some_and(|e| !e.is_null())
        || rec["detect"] != true
        || rec["read_ok"] == false
        || rec["secure_ok"] == false
        || rec["inserts"]
            .as_array()
            .is_some_and(|a| a.iter().any(|i| i["exact"] != true))
}

/// Record the error, screenshot a failing cell, and close the app.
fn finish(c: &mut Cell<'_>, r: Result<(), String>, screens: &Path) {
    if let Err(e) = r {
        let tail = c
            .running
            .as_ref()
            .map(Running::log_tail)
            .unwrap_or_default();
        let msg = if tail.is_empty() {
            e
        } else {
            format!("{e} (app log: {})", trunc(&tail, 300))
        };
        c.rec["error"] = json!(msg);
    }
    if cell_failed(c.rec) {
        let name = measure::cell_key(c.rec).replace(['/', '*'], "-");
        let path = screens.join(format!("{name}.png"));
        match launch::screenshot(c.cu, &path) {
            Ok(()) => c.rec["screenshot"] = json!(path.file_name().map(|f| f.to_string_lossy())),
            Err(e) => eprintln!("screenshot failed: {e}"),
        }
    }
    if let Some(mut r) = c.running.take() {
        r.kill();
    }
}

/// Poll `focused()` until `accept` takes the field or the deadline passes. Calls `nudge` (bring
/// to front, refocus) at 3 s and 10 s. Returns the accepted field, else the last non-secure
/// field `plausible` accepted, else the last result.
fn wait_field(
    c: &Cell<'_>,
    timeout: Duration,
    accept: impl Fn(&Focused) -> bool,
    plausible: impl Fn(&Focused) -> bool,
    mut nudge: impl FnMut(&Cell<'_>),
) -> (Option<Focused>, bool, String) {
    let start = Instant::now();
    let mut last_plausible: Option<Focused> = None;
    let mut last;
    let mut nudges = [Duration::from_secs(3), Duration::from_secs(10)]
        .into_iter()
        .peekable();
    loop {
        match c.a11y.focused() {
            Ok(Some(f)) => {
                if accept(&f) {
                    return (Some(f), true, String::new());
                }
                last = format!(
                    "{} {:?} role {} secure {} value {:?}",
                    f.app.name,
                    f.app.window_title,
                    f.field.role,
                    f.field.secure,
                    trunc(&f.field.value, 60)
                );
                if !f.field.secure && plausible(&f) {
                    last_plausible = Some(f);
                }
            }
            Ok(None) => last = "no focused text field".into(),
            Err(e) => last = format!("focused() error: {e}"),
        }
        if start.elapsed() > timeout {
            return (last_plausible, false, last);
        }
        if nudges.peek().is_some_and(|t| start.elapsed() > *t) {
            nudges.next();
            nudge(c);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// What the accessibility API sees of the desktop, for errors: apps (frontmost marked `*`)
/// and windows (focused marked `*`).
fn desktop(cu: &dyn ComputerUse) -> String {
    let apps = match cu.apps() {
        Ok(a) => a
            .iter()
            .take(8)
            .map(|a| format!("{}{}", a.name, if a.frontmost { "*" } else { "" }))
            .collect::<Vec<_>>()
            .join(", "),
        Err(e) => format!("error {e}"),
    };
    let windows = match cu.windows() {
        Ok(w) => w
            .iter()
            .take(8)
            .map(|w| {
                format!(
                    "{:?}{}",
                    trunc(w.title.as_deref().unwrap_or_default(), 50),
                    if w.focused { "*" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
        Err(e) => format!("error {e}"),
    };
    format!("apps: [{apps}]; windows: [{windows}]")
}

fn nudge_app(c: &Cell<'_>) {
    if let Some(r) = &c.running {
        eprintln!("  nudge: {}", launch::activate(c.cu, r));
    }
}

/// The window title of a fixture page: exactly `Nugget fixture: <name>`, alone (Safari) or
/// followed by the browser's suffix (` - Google Chrome`, ` — Mozilla Firefox`).
fn title_ok(f: &Focused, name: &str) -> bool {
    let want = format!("{TITLE}: {name}");
    f.app.window_title.as_deref().is_some_and(|t| {
        t == want
            || [" - ", " — ", " – "]
                .iter()
                .any(|sep| t.starts_with(&format!("{want}{sep}")))
    })
}

/// Whether the focused field is the fixture's editor (not the address bar, a search box or the
/// To and Subject fields): its accessible name is the editor's label, or it is a multi-line
/// text role on the fixture page.
fn is_editor(f: &Focused, ed: &Editor, label: &str) -> bool {
    if f.field.secure {
        return false;
    }
    if !label.is_empty() && f.field.label.as_deref() == Some(label) {
        return true;
    }
    let on_page = f
        .app
        .url
        .as_deref()
        .is_none_or(|u| u.contains(&format!("editor={}", ed.id)));
    on_page && matches!(f.field.role.as_str(), "text_area" | "document")
}

/// Five `focused()` calls: median and max milliseconds.
fn read_times(a11y: &dyn Accessibility) -> Value {
    let mut ms: Vec<f64> = (0..5)
        .map(|_| {
            let t = Instant::now();
            let _ = a11y.focused();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    measure::median_max(&mut ms)
}

fn set_field(rec: &mut Value, f: &Focused, detect: bool) {
    rec["detect"] = json!(detect);
    rec["role"] = json!(f.field.role);
    rec["label"] = json!(f.field.label);
    rec["seen"] = json!({
        "app": f.app.name,
        "app_id": f.app.id,
        "window_title": f.app.window_title.as_deref().map(|t| trunc(t, 120)),
        "url": f.app.url.as_deref().map(|u| trunc(u, 120)),
    });
}

/// Exact comparison form: only `\r\n` becomes `\n` (no trimming, no NBSP folding).
fn exact(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// The page's text model for exact comparisons: `text_raw` (NBSP kept) for plain editors,
/// where an NBSP would be a real difference, else `text`.
fn model_text(s: &Value, plain: bool) -> String {
    let raw = plain
        .then(|| s.get("text_raw").and_then(Value::as_str))
        .flatten();
    exact(raw.or(s["text"].as_str()).unwrap_or_default())
}

/// How long a matching text must stay unchanged before it counts: a second copy (a fallback
/// paste after an accessibility insert, a late key) shows up within this.
fn settle(payload: &str) -> Duration {
    Duration::from_millis(700 + (payload.chars().count() as u64 / 10).min(1300))
}

/// Wait until the page's text is exactly `want`, then for `settle`, and check it still is.
/// Returns whether it held, and the text last seen.
fn wait_exact(
    srv: &Server,
    edit: Option<&str>,
    plain: bool,
    want: &str,
    timeout: Duration,
    settle: Duration,
) -> (bool, String) {
    match srv.wait_state(edit, timeout, |s| model_text(s, plain) == want) {
        Ok(_) => {
            std::thread::sleep(settle);
            let now = srv
                .state(edit)
                .map(|s| model_text(&s, plain))
                .unwrap_or_default();
            (now == want, now)
        }
        Err(l) => (false, l.map(|s| model_text(&s, plain)).unwrap_or_default()),
    }
}

#[allow(clippy::too_many_arguments)]
fn web_cell(
    c: &mut Cell<'_>,
    srv: &Server,
    truth: &Truth,
    ed: &Editor,
    spec: &AppSpec,
    exe: &Path,
    dir: &Path,
    force_a11y: bool,
) -> Result<(), String> {
    let edit = Some(ed.id.as_str());
    let plain = ed.kind == "plain";
    let initial = truth.initial_text.as_str();
    srv.forget_page();
    let url = srv.editor_url(&ed.id);
    c.running = Some(launch::launch_browser(*spec, exe, &url, dir, force_a11y)?);

    // The page loads and takes the first reset (browser start-up can be slow on a cold runner).
    let seq = srv.send("reset", Some(initial));
    srv.wait_ack(edit, seq, Duration::from_secs(90))
        .map_err(|last| {
            format!(
                "the page never acknowledged reset (index served: {}, /cmd polls: {}, last state: \
                 {last:?}, app alive: {:?})",
                srv.served("/index.html"),
                srv.polls(),
                c.running.as_mut().and_then(Running::alive)
            )
        })?;
    srv.wait_state(edit, Duration::from_secs(5), |s| s["active"] == "editor")
        .map_err(|last| format!("the editor did not take focus after reset: {last:?}"))?;
    let label = srv
        .state(edit)
        .and_then(|s| s["label"].as_str().map(str::to_string))
        .unwrap_or_default();

    // The browser in front and the editor focused, as accessibility sees it.
    let oracle = || {
        srv.state(edit)
            .map(|s| model_text(&s, plain))
            .unwrap_or_default()
    };
    let (f, matched, last) = wait_field(
        c,
        Duration::from_secs(25),
        |f| {
            title_ok(f, &ed.name)
                && is_editor(f, ed, &label)
                && norm(&f.field.value) == norm(&oracle())
        },
        |f| title_ok(f, &ed.name) && is_editor(f, ed, &label),
        |c| {
            nudge_app(c);
            let s = srv.send("focus-editor", None);
            let _ = srv.wait_ack(edit, s, Duration::from_secs(3));
        },
    );
    let Some(f) = f else {
        c.rec["detect"] = json!(false);
        return Err(format!(
            "the editor never had accessibility focus in the fixture window within 25 s (last: \
             {last}; {})",
            desktop(c.cu)
        ));
    };
    set_field(c.rec, &f, true);
    let text = oracle();
    c.rec["read_ok"] = json!(matched);
    if !matched {
        c.rec["read_diff"] = json!({
            "expected": trunc(&norm(&text), 200),
            "got": trunc(&norm(&f.field.value), 200),
        });
    }

    // Thread, recipients, subject.
    let got: Vec<GotMessage> = f
        .thread
        .iter()
        .map(|m| GotMessage {
            text: m.text.clone(),
            from_me: m.from_me,
        })
        .collect();
    let mut thread = measure::score_thread(&got, &truth.messages(&ed.context));
    thread["recipients_ok"] = json!(measure::recipients_ok(
        &f.recipients,
        &truth.recipients(&ed.context)
    ));
    thread["subject_ok"] = json!(measure::subject_ok(
        f.subject.as_deref(),
        truth.subject(&ed.context).as_deref()
    ));
    if thread["precision"] != json!(1.0) || thread["recall"] != json!(1.0) {
        thread["got_texts"] = json!(
            f.thread
                .iter()
                .take(12)
                .map(|m| trunc(&m.text, 80))
                .collect::<Vec<_>>()
        );
    }
    if thread["recipients_ok"] == false || thread["subject_ok"] == false {
        thread["got_recipients"] = json!(f.recipients);
        thread["got_subject"] = json!(f.subject);
    }
    c.rec["thread"] = thread;
    if let Some(dir) = std::env::var_os("NUGGET_A11Y_APPS_RECORD") {
        let name = format!("web_{}_{}_{}.json", spec.id, os_id(), ed.id);
        match record_snapshot(&Path::new(&dir).join(&name)) {
            Ok(()) => eprintln!("  recorded {name}"),
            Err(e) => eprintln!("  recording {name} failed: {e}"),
        }
    }
    c.rec["read_ms"] = read_times(c.a11y);

    // Secure field.
    c.rec["secure_ok"] = json!(secure_check(c, srv, edit)?);

    // Insertions, each followed by one undo.
    let mut inserts = Vec::new();
    for (pid, payload) in measure::payloads() {
        let r = web_insert(c, srv, edit, plain, initial, pid, &payload);
        inserts.push(r);
        c.rec["inserts"] = json!(inserts);
    }
    Ok(())
}

/// `NUGGET_A11Y_APPS_RECORD`: the focused window's snapshot (`Backend::snapshot`) as JSON, for
/// the thread fixtures in `tests/fixtures/trees/`.
fn record_snapshot(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let backend =
        nugget_a11y::Backend::new(nugget_a11y::AtspiPlatform::new(), A11yConfig::default());
    #[cfg(target_os = "macos")]
    let backend = nugget_a11y::Backend::new(nugget_a11y::AxPlatform::new(), A11yConfig::default());
    #[cfg(windows)]
    let backend = nugget_a11y::Backend::new(nugget_a11y::UiaPlatform::new(), A11yConfig::default());
    let (_, snap) = backend
        .snapshot()
        .map_err(|e| e.to_string())?
        .ok_or("no focused window")?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, snap.to_json()).map_err(|e| e.to_string())
}

/// `focus-password`: `focused()` must report a secure, empty field, `insert` must refuse, and
/// the page's password length must not change. `None` (a harness error, recorded) when the
/// page could not focus its password field.
fn secure_check(
    c: &mut Cell<'_>,
    srv: &Server,
    edit: Option<&str>,
) -> Result<Option<bool>, String> {
    let seq = srv.send("focus-password", None);
    srv.wait_ack(edit, seq, Duration::from_secs(5))
        .map_err(|l| format!("focus-password not acknowledged: {l:?}"))?;
    let before = match srv.wait_state(edit, Duration::from_secs(3), |s| s["active"] == "password") {
        Ok(s) => s,
        Err(l) => {
            c.rec["secure"] = json!({
                "error": format!("harness: the page did not focus its password field ({l:?})"),
            });
            return Ok(None);
        }
    };
    let len_before = before["password_len"].clone();
    let (f, ok, last) = wait_field(
        c,
        Duration::from_secs(8),
        |f| f.field.secure,
        |_| false,
        |_| {},
    );
    let (secure, empty) = match (&f, ok) {
        (Some(f), true) => (true, f.field.value.is_empty()),
        _ => (false, false),
    };
    let refused = c.a11y.insert("x").is_err();
    std::thread::sleep(Duration::from_millis(500));
    let seq = srv.send("focus-password", None);
    let after = srv
        .wait_ack(edit, seq, Duration::from_secs(5))
        .map_err(|l| format!("focus-password not acknowledged: {l:?}"))?;
    let unchanged = after["password_len"] == len_before && !len_before.is_null();
    c.rec["secure"] = json!({
        "secure": secure,
        "value_empty": empty,
        "insert_refused": refused,
        "password_len_unchanged": unchanged,
        "last": if ok { Value::Null } else { json!(last) },
    });
    Ok(Some(secure && empty && refused && unchanged))
}

fn web_insert(
    c: &mut Cell<'_>,
    srv: &Server,
    edit: Option<&str>,
    plain: bool,
    initial: &str,
    pid: &str,
    payload: &str,
) -> Value {
    let mut r = json!({"payload": pid, "method": null, "verified": null, "exact": false,
                       "undo_one_step": null});
    let seq = srv.send("reset", Some(initial));
    let pre = match srv.wait_ack(edit, seq, Duration::from_secs(5)) {
        Ok(s) => model_text(&s, plain),
        Err(l) => {
            r["error"] = json!(format!("reset not acknowledged: {l:?}"));
            return r;
        }
    };
    let _ = srv.wait_state(edit, Duration::from_secs(3), |s| s["active"] == "editor");
    // Accessibility sees the reset field again before inserting.
    let (_, seen, last) = wait_field(
        c,
        Duration::from_secs(5),
        |f| !f.field.secure && norm(&f.field.value) == norm(&pre),
        |_| false,
        |_| {},
    );
    if !seen {
        r["note"] = json!(format!("field not re-read after reset: {last}"));
    }
    let expected = format!("{pre}{payload}");
    let t = Instant::now();
    match c.a11y.insert(payload) {
        Ok(o) => {
            r["method"] = json!(format!("{:?}", o.method).to_lowercase());
            r["verified"] = json!(o.verified);
        }
        Err(e) => {
            r["verified"] = json!(false);
            r["error"] = json!(trunc(&e.to_string(), 200));
        }
    }
    r["insert_ms"] = json!((t.elapsed().as_secs_f64() * 1000.0).round());
    let (ok, got) = wait_exact(
        srv,
        edit,
        plain,
        &expected,
        Duration::from_secs(3),
        settle(payload),
    );
    r["exact"] = json!(ok);
    if !ok {
        r["diff"] = json!(measure::diff(&expected, &got));
    }
    if got == pre {
        // Nothing was inserted: nothing to undo.
        return r;
    }
    if let Err(e) = c.cu.key_input(None, &undo_chord()) {
        r["undo_error"] = json!(trunc(&e.to_string(), 200));
    }
    let (back, now) = wait_exact(srv, edit, plain, &pre, Duration::from_secs(2), settle(""));
    r["undo_one_step"] = json!(back);
    if !back {
        r["undo_text"] = json!(trunc(&now, 120));
    }
    r
}

/// TextEdit's own text (AppleScript), independent of accessibility. One try per run with a
/// short timeout: on a runner the first call can sit on an Automation consent prompt.
fn textedit_text() -> Option<String> {
    static USABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    if !USABLE.load(std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    let r = launch::run_out(
        "/usr/bin/osascript",
        &[
            "-e",
            "tell application \"TextEdit\" to get text of document 1",
        ],
        Duration::from_secs(3),
    );
    match r {
        Ok(s) => Some(s.strip_suffix('\n').unwrap_or(&s).to_string()),
        Err(e) => {
            eprintln!("  osascript oracle disabled for this run: {e}");
            USABLE.store(false, std::sync::atomic::Ordering::SeqCst);
            None
        }
    }
}

fn native_cell(
    c: &mut Cell<'_>,
    spec: &AppSpec,
    n: Native,
    exe: &Path,
    dir: &Path,
    fixtures: &Path,
) -> Result<(), String> {
    let seed_src = fixtures.join("native").join("seed.txt");
    let seed = std::fs::read_to_string(&seed_src).unwrap_or_else(|_| SEED.to_string());
    let doc = dir.join("nugget-draft.txt");
    std::fs::write(&doc, &seed).map_err(|e| format!("seed copy: {e}"))?;
    c.running = Some(launch::launch_native(*spec, exe, &doc, dir)?);

    if n == Native::Notes {
        // Best effort: a new note, then the seed typed into it.
        std::thread::sleep(Duration::from_secs(4));
        nudge_app(c);
        let _ = c.cu.key_input(None, &keys("cmd+n"));
        std::thread::sleep(Duration::from_secs(1));
        let _ = c.cu.key_input(None, &KeyInput::Text(SEED.into()));
    }

    let (f, matched, last) = wait_field(
        c,
        Duration::from_secs(45),
        |f| !f.field.secure && norm(&f.field.value).contains(SEED.trim_end()),
        |_| false,
        nudge_app,
    );
    let Some(f) = f.filter(|_| matched) else {
        c.rec["detect"] = json!(false);
        return Err(format!(
            "no focused field with the seed text within 45 s (last: {last}; {})",
            desktop(c.cu)
        ));
    };
    set_field(c.rec, &f, true);
    let _ = c.cu.key_input(None, &end_of_document());
    std::thread::sleep(Duration::from_millis(300));

    // The oracle: TextEdit's AppleScript text when it answers, else the a11y read-back.
    let use_script = n == Native::TextEdit && textedit_text().is_some();
    if n == Native::TextEdit && !use_script {
        c.rec["oracle"] = json!("a11y");
        c.rec["oracle_note"] = json!("osascript unavailable (timed out or refused)");
    }
    let a11y = c.a11y;
    let read_oracle = move || -> Option<String> {
        if use_script {
            textedit_text()
        } else {
            a11y.focused().ok().flatten().map(|f| exact(&f.field.value))
        }
    };
    // Exactly `want`, still so after `settle`. `None`: the oracle could not be read.
    let wait_oracle = |want: &str, timeout: Duration, settle: Duration| -> Option<(bool, String)> {
        let deadline = Instant::now() + timeout;
        loop {
            let got = read_oracle()?;
            if got == want {
                std::thread::sleep(settle);
                let now = read_oracle()?;
                return Some((now == want, now));
            }
            if Instant::now() > deadline {
                return Some((false, got));
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    };

    let value = exact(&f.field.value);
    let read_ok = if use_script {
        read_oracle().is_some_and(|o| o == value)
    } else {
        value == seed
    };
    c.rec["read_ok"] = json!(read_ok);
    if !read_ok {
        c.rec["read_diff"] = json!({"expected": trunc(&seed, 200), "got": trunc(&value, 200)});
    }
    c.rec["read_ms"] = read_times(c.a11y);

    let mut inserts = Vec::new();
    for (pid, payload) in measure::payloads() {
        let mut r = json!({"payload": pid, "method": null, "verified": null, "exact": false,
                           "undo_one_step": null});
        let r = 'cell: {
            // Back to the seed when the previous undo did not get there.
            let at_seed = wait_oracle(&seed, Duration::from_millis(300), Duration::ZERO);
            if !at_seed.as_ref().is_some_and(|x| x.0) {
                let _ = c.cu.key_input(None, &select_all());
                let _ = c.cu.key_input(None, &KeyInput::Text(seed.clone()));
                let _ = c.cu.key_input(None, &end_of_document());
                match wait_oracle(&seed, Duration::from_secs(3), settle("")) {
                    Some((true, _)) => {}
                    other => {
                        r["error"] = json!(format!(
                            "could not reset the document to the seed (it reads {:?})",
                            other.map(|o| trunc(&o.1, 80))
                        ));
                        break 'cell r;
                    }
                }
            }
            let Some(pre) = read_oracle() else {
                r["error"] = json!("oracle unavailable before the insert");
                break 'cell r;
            };
            let expected = format!("{pre}{payload}");
            let t = Instant::now();
            match c.a11y.insert(&payload) {
                Ok(o) => {
                    r["method"] = json!(format!("{:?}", o.method).to_lowercase());
                    r["verified"] = json!(o.verified);
                }
                Err(e) => {
                    r["verified"] = json!(false);
                    r["error"] = json!(trunc(&e.to_string(), 200));
                }
            }
            r["insert_ms"] = json!((t.elapsed().as_secs_f64() * 1000.0).round());
            let Some((ok, got)) = wait_oracle(&expected, Duration::from_secs(3), settle(&payload))
            else {
                r["error"] = json!("oracle unavailable after the insert");
                break 'cell r;
            };
            r["exact"] = json!(ok);
            if !ok {
                r["diff"] = json!(measure::diff(&expected, &got));
            }
            if got != pre {
                if let Err(e) = c.cu.key_input(None, &undo_chord()) {
                    r["undo_error"] = json!(trunc(&e.to_string(), 200));
                }
                match wait_oracle(&pre, Duration::from_secs(2), settle("")) {
                    Some((back, now)) => {
                        r["undo_one_step"] = json!(back);
                        if !back {
                            r["undo_text"] = json!(trunc(&now, 120));
                        }
                    }
                    None => r["undo_error"] = json!("oracle unavailable after the undo"),
                }
            }
            r
        };
        inserts.push(r);
        c.rec["inserts"] = json!(inserts);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truth_parsing() {
        let t = Truth::parse(
            r#"{"version":1,"initial_text":"Draft: ",
                "editors":[{"id":"textarea","name":"Plain textarea","context":"chat","kind":"plain"},
                           {"id":"mail-compose","name":"Mail","context":"mail","kind":"rich"}],
                "contexts":{"chat":{"messages":[{"author":"Maya Chen","time":"10:02 AM",
                                                 "text":"Hi there friend","from_me":false}],
                                    "recipients":[],"subject":null},
                            "mail":{"messages":[],"recipients":["Jordan Lee"],
                                    "subject":"Q3 offsite venue"}}}"#,
        )
        .unwrap();
        assert_eq!(t.initial_text, "Draft: ");
        assert_eq!(t.editors.len(), 2);
        assert_eq!(t.editors[1].context, "mail");
        assert_eq!(t.messages("chat").len(), 1);
        assert_eq!(t.recipients("mail"), ["Jordan Lee"]);
        assert_eq!(t.subject("mail").as_deref(), Some("Q3 offsite venue"));
        assert_eq!(t.subject("chat"), None);
        assert!(t.messages("nope").is_empty());
        assert!(Truth::parse("{}").is_err());
        assert!(Truth::parse("nope").is_err());
    }

    #[test]
    fn records_and_failure_detection() {
        let env = Env {
            os: os_id(),
            os_version: "x".into(),
            runner: Some("r".into()),
            force_a11y: true,
        };
        let spec = launch::all_apps()[0];
        let mut rec = env.record(&spec, Some("1.0"), "quill");
        assert_eq!(rec["app"], spec.id);
        assert_eq!(rec["force_a11y"], spec.is_chromium());
        assert!(cell_failed(&rec), "not detected yet");
        rec["detect"] = json!(true);
        rec["read_ok"] = json!(true);
        rec["inserts"] = json!([{"payload": "ascii", "exact": true}]);
        assert!(!cell_failed(&rec));
        rec["inserts"] = json!([{"payload": "ascii", "exact": false}]);
        assert!(cell_failed(&rec));
        assert!(summary(&rec).contains("inserts=0/1"));
    }

    #[test]
    fn checked_in_config_parses() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("apps");
        let exp: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap())
                .unwrap();
        let map = exp.as_object().expect("expected.json is an object");
        for (key, metrics) in map {
            assert_eq!(key.split('/').count(), 3, "{key}");
            for m in metrics.as_array().expect("a list") {
                let m = m.as_str().expect("metric name");
                let probe = json!({"available": true, "inserts": [], "thread": {}});
                assert!(
                    measure::metric_passes(&probe, m).is_some(),
                    "{key}: unknown metric {m}"
                );
            }
        }
        let seed = std::fs::read_to_string(dir.join("native").join("seed.txt")).unwrap();
        assert_eq!(seed, SEED);
    }
}
