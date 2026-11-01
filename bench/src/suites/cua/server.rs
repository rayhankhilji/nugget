//! The apps' local HTTP server (loopback only, std only), and a tiny client for it.
//!
//! | Route | Use |
//! |---|---|
//! | `GET /<app>/` | The app window as an HTML page (what kiosk Chromium shows) |
//! | `POST /<app>/act` | An [`Action`] on that app, from the page's own script |
//! | `GET /__agent/observe` | The accessibility tree of every open window |
//! | `POST /__agent/act` | An [`Action`] on any open app |
//! | `POST /__agent/confirm` | An `ActionDescriptor`; answers `{"approved": bool}` |
//! | `POST /__agent/ask` | `{"question", "options"}`: a clarifying question; answers `{"answer": string or null}` |
//! | `GET /__test/state` | **Test only.** Every app's internal state, the event log and confirmations |
//! | `GET /__test/version` | Changes whenever the world does; pages poll it to refresh |
//!
//! `/__test/*` is the outcome oracle. Agents under test must not read it.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use nugget_policy::ActionDescriptor;
use serde_json::{Value, json};

use super::agent::ActResult;
use super::apps::APP_IDS;
use super::ui::{Action, render_page};
use super::world::World;

const MAX_BODY: usize = 1 << 20;

/// Holds the world of the task being run, shared by the runner, the server and transports.
#[derive(Default)]
pub struct Stage {
    world: Mutex<Option<World>>,
    epoch: AtomicU64,
    pages: Mutex<BTreeSet<String>>,
    /// Per app: the world version its page was last served at, and when.
    served: Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Stage {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Install the next task's world.
    pub fn load(&self, world: World) {
        *lock(&self.world) = Some(world);
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Remove the world for scoring.
    pub fn take(&self) -> Option<World> {
        let w = lock(&self.world).take();
        self.epoch.fetch_add(1, Ordering::SeqCst);
        w
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut World) -> R) -> Result<R, String> {
        let mut g = lock(&self.world);
        let w = g.as_mut().ok_or("no task is loaded")?;
        Ok(f(w))
    }

    pub fn version(&self) -> String {
        let v = lock(&self.world).as_ref().map_or(0, World::version);
        format!("{}:{v}", self.epoch.load(Ordering::SeqCst))
    }

    /// App pages served at least once (display readiness).
    pub fn pages_served(&self) -> BTreeSet<String> {
        lock(&self.pages).clone()
    }

    /// The version `app`'s page was last served at, and when (a display page that is current).
    pub fn page_served(&self, app: &str) -> Option<(String, std::time::Instant)> {
        lock(&self.served).get(app).cloned()
    }

    /// Apply an action and translate the outcome for agents.
    pub fn act(&self, action: &Action) -> Result<ActResult, String> {
        self.with(|w| match w.act(action) {
            Ok(_) => ActResult {
                ok: true,
                message: None,
            },
            Err(m) => ActResult {
                ok: false,
                message: Some(m),
            },
        })
    }
}

pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Server {
    /// Listen on an ephemeral loopback port.
    pub fn start(stage: Arc<Stage>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::Builder::new()
            .name("cua-apps".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(conn) = conn else { continue };
                    let stage = stage.clone();
                    let _ = std::thread::Builder::new()
                        .name("cua-conn".into())
                        .spawn(move || {
                            let _ = serve(conn, &stage);
                        });
                }
            })?;
        Ok(Self {
            addr,
            stop,
            handle: Some(handle),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &TcpStream) -> std::io::Result<Request> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    if len > MAX_BODY {
        return Err(std::io::Error::other("body too large"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok(Request { method, path, body })
}

fn respond(mut stream: &TcpStream, status: u16, ctype: &str, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn serve(stream: TcpStream, stage: &Stage) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let req = read_request(&stream)?;
    let (status, ctype, body) = route(&req, stage);
    respond(&stream, status, ctype, &body)
}

const JSON: &str = "application/json";

fn json_reply(v: Result<Value, String>) -> (u16, &'static str, String) {
    match v {
        Ok(v) => (200, JSON, v.to_string()),
        Err(e) => (400, JSON, json!({ "error": e }).to_string()),
    }
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, String> {
    serde_json::from_slice(body).map_err(|e| format!("bad request body: {e}"))
}

fn act_reply(
    stage: &Stage,
    action: Result<Action, String>,
    app: Option<&str>,
) -> Result<Value, String> {
    let action = action?;
    if let Some(app) = app
        && action.app() != app
    {
        return Err(format!("`{}` is not in {app}", action.target()));
    }
    let r = stage.act(&action)?;
    serde_json::to_value(r).map_err(|e| e.to_string())
}

/// The body of `POST /__agent/ask`.
#[derive(serde::Deserialize)]
struct AskBody {
    question: String,
    #[serde(default)]
    options: Vec<String>,
}

fn route(req: &Request, stage: &Stage) -> (u16, &'static str, String) {
    let path = req.path.split('?').next().unwrap_or_default();
    match (req.method.as_str(), path) {
        ("GET", "/") => {
            let links: String = APP_IDS
                .iter()
                .map(|a| format!("<li><a href=\"/{a}/\">{a}</a></li>"))
                .collect();
            (
                200,
                "text/html; charset=utf-8",
                format!("<!doctype html><title>Nugget cua apps</title><ul>{links}</ul>"),
            )
        }
        ("GET", "/__test/version") => (200, "text/plain", stage.version()),
        ("GET", "/__test/state") => json_reply(stage.with(|w| w.snapshot())),
        ("GET", "/__agent/observe") => json_reply(
            stage
                .with(|w| serde_json::to_value(w.observe()))
                .and_then(|r| r.map_err(|e| e.to_string())),
        ),
        ("POST", "/__agent/act") => json_reply(act_reply(stage, parse(&req.body), None)),
        ("POST", "/__agent/confirm") => json_reply(
            parse::<ActionDescriptor>(&req.body)
                .and_then(|d| stage.with(|w| json!({ "approved": w.confirm(&d) }))),
        ),
        ("POST", "/__agent/ask") => json_reply(
            parse::<AskBody>(&req.body)
                .and_then(|b| stage.with(|w| json!({ "answer": w.ask(&b.question, &b.options) }))),
        ),
        (method, p) => {
            let mut segs = p.trim_matches('/').splitn(2, '/');
            let app = segs.next().unwrap_or_default();
            let rest = segs.next().unwrap_or_default();
            if !APP_IDS.contains(&app) {
                return (404, "text/plain", "not found".into());
            }
            match (method, rest) {
                ("GET", "") => {
                    lock(&stage.pages).insert(app.to_owned());
                    let version = stage.version();
                    lock(&stage.served)
                        .insert(app.to_owned(), (version.clone(), std::time::Instant::now()));
                    let window = stage.with(|w| w.window(app)).ok().flatten();
                    (
                        200,
                        "text/html; charset=utf-8",
                        render_page(app, window.as_ref(), &version),
                    )
                }
                ("POST", "act") => json_reply(act_reply(stage, parse(&req.body), Some(app))),
                _ => (404, "text/plain", "not found".into()),
            }
        }
    }
}

/// Minimal HTTP/1.1 client for the loopback server. Returns the status and body.
pub fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, String), String> {
    let mut s =
        TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    let body = body.unwrap_or("");
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .map_err(|e| e.to_string())?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| e.to_string())?;
    let (head, body) = out
        .split_once("\r\n\r\n")
        .ok_or("malformed HTTP response")?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or("malformed status line")?;
    Ok((status, body.to_owned()))
}

/// `request`, expecting 200 and a JSON body.
pub fn request_json(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let text = body.map(Value::to_string);
    let (status, out) = request(addr, method, path, text.as_deref())?;
    let v: Value = serde_json::from_str(&out).map_err(|e| format!("{path}: {e}"))?;
    if status != 200 {
        return Err(format!(
            "{path}: HTTP {status}: {}",
            v.get("error").and_then(Value::as_str).unwrap_or("")
        ));
    }
    Ok(v)
}
