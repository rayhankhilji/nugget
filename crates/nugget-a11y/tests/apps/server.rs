//! A small std-only HTTP/1.1 server for the web fixtures (`tests/fixtures/apps/PROTOCOL.md`):
//! static files from `web/`, `GET /truth.json`, the command channel (`GET /cmd`) and the state
//! reports (`POST /state`). One thread per connection, `Connection: close`, loopback only.
//!
//! The harness side is [`Server::send`] (queue a command, returns its `seq`),
//! [`Server::wait_ack`] (the first state that acknowledges it) and [`Server::wait_state`].

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 4 * 1024 * 1024;
const LOG_CAP: usize = 4000;

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub seq: u64,
    pub cmd: String,
    pub text: Option<String>,
}

#[derive(Default)]
struct Inner {
    seq: u64,
    cmd: Option<Command>,
    state: Option<Value>,
    /// The latest state per `editor` (a page left over from an earlier cell can still post).
    by_editor: HashMap<String, Value>,
    /// Every state report, with the time it arrived (since the server started).
    log: VecDeque<(Duration, Value)>,
    /// Paths of the files served (for "did the page load").
    served: Vec<String>,
    polls: u64,
    /// The page this server follows: the first `token` seen since the last `forget_page`.
    token: Option<String>,
    /// The highest `n` of that page's state posts.
    last_n: u64,
}

impl Inner {
    /// Whether a request carrying `token` comes from the followed page (binding the first one).
    /// Requests without a token (an older page) are always accepted.
    fn follows(&mut self, token: Option<&str>) -> bool {
        match (token, &self.token) {
            (None, _) => true,
            (Some(t), None) => {
                self.token = Some(t.to_string());
                true
            }
            (Some(t), Some(bound)) => t == bound,
        }
    }
}

struct Shared {
    root: PathBuf,
    started: Instant,
    inner: Mutex<Inner>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The fixture server; stops on drop.
pub struct Server {
    addr: SocketAddr,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Serve `root` (the directory holding `web/` and `truth.json`) on `127.0.0.1:0`.
    pub fn start(root: &Path) -> std::io::Result<Server> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            root: root.to_path_buf(),
            started: Instant::now(),
            inner: Mutex::new(Inner::default()),
            cv: Condvar::new(),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let shared = shared.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("apps-http".into())
                .spawn(move || {
                    for conn in listener.incoming() {
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let Ok(stream) = conn else { continue };
                        let shared = shared.clone();
                        let _ = std::thread::Builder::new()
                            .name("apps-http-conn".into())
                            .spawn(move || {
                                let _ = handle(&shared, stream);
                            });
                    }
                })?
        };
        Ok(Server {
            addr,
            shared,
            stop,
            thread: Some(thread),
        })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    /// The URL of a fixture page for one editor.
    pub fn editor_url(&self, editor: &str) -> String {
        format!("{}/index.html?editor={editor}", self.base_url())
    }

    /// Queue `cmd` (replacing any command the page has not picked up) and return its `seq`.
    pub fn send(&self, cmd: &str, text: Option<&str>) -> u64 {
        let mut g = self.shared.lock();
        g.seq += 1;
        let seq = g.seq;
        g.cmd = Some(Command {
            seq,
            cmd: cmd.to_string(),
            text: text.map(str::to_string),
        });
        seq
    }

    /// Forget the pending command and the last state (before opening a new page), keeping the
    /// `seq` counter monotonic so a new page never sees an old command.
    pub fn forget_page(&self) {
        let mut g = self.shared.lock();
        g.cmd = None;
        g.state = None;
        g.by_editor.clear();
        g.served.clear();
        g.token = None;
        g.last_n = 0;
    }

    /// The latest state report (of `editor`, when given).
    pub fn state(&self, editor: Option<&str>) -> Option<Value> {
        let g = self.shared.lock();
        match editor {
            Some(e) => g.by_editor.get(e).cloned(),
            None => g.state.clone(),
        }
    }

    /// Every state report so far (time since the server started, body).
    pub fn log(&self) -> Vec<(Duration, Value)> {
        self.shared.lock().log.iter().cloned().collect()
    }

    /// Whether a path (e.g. `/index.html`) was served since the last [`forget_page`].
    pub fn served(&self, path: &str) -> bool {
        self.shared.lock().served.iter().any(|p| p == path)
    }

    /// How many `GET /cmd` polls the server answered.
    pub fn polls(&self) -> u64 {
        self.shared.lock().polls
    }

    /// Wait until the latest state (of `editor`, when given) satisfies `pred`; returns it, or
    /// that latest state on timeout.
    pub fn wait_state(
        &self,
        editor: Option<&str>,
        timeout: Duration,
        pred: impl Fn(&Value) -> bool,
    ) -> Result<Value, Option<Value>> {
        let deadline = Instant::now() + timeout;
        let mut g = self.shared.lock();
        loop {
            let latest = match editor {
                Some(e) => g.by_editor.get(e),
                None => g.state.as_ref(),
            };
            if let Some(s) = latest
                && pred(s)
            {
                return Ok(s.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(latest.cloned());
            }
            g = self
                .shared
                .cv
                .wait_timeout(g, left.min(Duration::from_millis(200)))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Wait for a state (of `editor`, when given) with `ack >= seq`.
    pub fn wait_ack(
        &self,
        editor: Option<&str>,
        seq: u64,
        timeout: Duration,
    ) -> Result<Value, Option<Value>> {
        self.wait_state(editor, timeout, |s| {
            s.get("ack")
                .and_then(Value::as_u64)
                .is_some_and(|a| a >= seq)
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock `accept`.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

impl Request {
    /// A query parameter (no percent-decoding: tokens are alphanumeric).
    fn param(&self, name: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == name && !v.is_empty()).then_some(v)
        })
    }
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return Ok(None);
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_string();
    let target = first.next().unwrap_or_default().to_string();
    let mut len = 0usize;
    for l in lines {
        if let Some((k, v)) = l.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    if len > MAX_BODY {
        return Ok(None);
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    let target = target.split('#').next().unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Some(Request {
        method,
        path: path.to_string(),
        query: query.to_string(),
        body,
    }))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

pub fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// `%XX` decoding of a URL path; `None` when malformed or not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Windows device names (`CON`, `nul.txt`, `COM1`, ...), which open devices in any directory.
fn is_device_name(seg: &str) -> bool {
    let stem = seg.split('.').next().unwrap_or_default();
    let stem = stem.trim_end_matches([' ', '.']).to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit())
}

/// The file under `base` for a URL path, refusing anything that could leave `base`.
pub fn resolve(base: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path)?;
    let mut p = base.to_path_buf();
    let mut any = false;
    for seg in decoded.split('/').filter(|s| !s.is_empty()) {
        if seg.starts_with('.') || seg.contains(['\\', ':', '\0']) || is_device_name(seg) {
            return None;
        }
        p.push(seg);
        any = true;
    }
    if !any {
        p.push("index.html");
    }
    Some(p)
}

fn handle(shared: &Shared, mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let Some(req) = read_request(&mut stream)? else {
        return respond(&mut stream, "400 Bad Request", "text/plain", b"bad request");
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/cmd") => {
            let body = {
                let mut g = shared.lock();
                g.polls += 1;
                let follows = g.follows(req.param("token"));
                match &g.cmd {
                    Some(c) if follows => {
                        let mut v = json!({"seq": c.seq, "cmd": c.cmd});
                        if let Some(t) = &c.text {
                            v["text"] = json!(t);
                        }
                        v
                    }
                    _ => json!({"seq": g.seq}),
                }
            };
            respond(
                &mut stream,
                "200 OK",
                content_type(".json"),
                body.to_string().as_bytes(),
            )
        }
        ("POST", "/state") => match serde_json::from_slice::<Value>(&req.body) {
            Ok(v) if v.is_object() => {
                let mut g = shared.lock();
                let follows = g.follows(v.get("token").and_then(Value::as_str));
                let n = v.get("n").and_then(Value::as_u64);
                // Another page, or a post overtaken by a newer one: ignored.
                if follows && n.is_none_or(|n| n > g.last_n) {
                    if let Some(n) = n {
                        g.last_n = n;
                    }
                    let at = shared.started.elapsed();
                    g.log.push_back((at, v.clone()));
                    if let Some(e) = v.get("editor").and_then(Value::as_str) {
                        g.by_editor.insert(e.to_string(), v.clone());
                    }
                    while g.log.len() > LOG_CAP {
                        g.log.pop_front();
                    }
                    g.state = Some(v);
                }
                drop(g);
                shared.cv.notify_all();
                respond(&mut stream, "204 No Content", "text/plain", b"")
            }
            _ => respond(&mut stream, "400 Bad Request", "text/plain", b"bad state"),
        },
        ("OPTIONS", _) => respond(&mut stream, "204 No Content", "text/plain", b""),
        ("GET" | "HEAD", path) => {
            let web = shared.root.join("web");
            let file = if path == "/truth.json" {
                Some(shared.root.join("truth.json"))
            } else {
                // Resolved, then canonicalised: it must still be inside `web/` (no symlink out).
                resolve(&web, path).filter(|f| match (f.canonicalize(), web.canonicalize()) {
                    (Ok(f), Ok(w)) => f.starts_with(w),
                    _ => false,
                })
            };
            match file.and_then(|f| Some((std::fs::read(&f).ok()?, f))) {
                Some((bytes, f)) => {
                    shared.lock().served.push(path.to_string());
                    let body: &[u8] = if req.method == "HEAD" { &[] } else { &bytes };
                    let ctype = content_type(&f.to_string_lossy());
                    respond(&mut stream, "200 OK", ctype, body)
                }
                None => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
            }
        }
        _ => respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp fixtures dir with `web/` and `truth.json`, removed on drop.
    struct Root(PathBuf);

    impl Root {
        fn new(name: &str) -> Root {
            let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
                .join(format!("apps-server-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let web = dir.join("web");
            std::fs::create_dir_all(web.join("lib")).unwrap();
            std::fs::write(web.join("index.html"), "<!doctype html><title>x</title>").unwrap();
            std::fs::write(web.join("lib").join("a.js"), "console.log(1)").unwrap();
            std::fs::write(web.join("s.css"), "body{}").unwrap();
            std::fs::write(web.join("i.svg"), "<svg/>").unwrap();
            std::fs::write(dir.join("truth.json"), r#"{"version":1}"#).unwrap();
            std::fs::write(dir.join("secret.txt"), "no").unwrap();
            Root(dir)
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One request over a plain `TcpStream`: (status code, headers, body).
    fn request(port: u16, method: &str, path: &str, body: &str) -> (u16, String, Vec<u8>) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        let end = find(&out, b"\r\n\r\n").expect("response head");
        let head = String::from_utf8_lossy(&out[..end]).into_owned();
        let code = head.split(' ').nth(1).unwrap().parse().unwrap();
        (code, head, out[end + 4..].to_vec())
    }

    fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
        head.split("\r\n").find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }

    #[test]
    fn serves_static_files_with_content_types() {
        let root = Root::new("static");
        let srv = Server::start(&root.0).unwrap();
        let p = srv.port();
        for (path, ctype, body) in [
            ("/index.html?editor=quill", "text/html", "<!doctype html>"),
            ("/", "text/html", "<!doctype html>"),
            ("/lib/a.js", "text/javascript", "console.log"),
            ("/s.css", "text/css", "body"),
            ("/i.svg", "image/svg+xml", "<svg"),
            ("/truth.json", "application/json", r#"{"version":1}"#),
        ] {
            let (code, head, got) = request(p, "GET", path, "");
            assert_eq!(code, 200, "{path}: {head}");
            assert!(
                header(&head, "content-type").unwrap().starts_with(ctype),
                "{path}: {head}"
            );
            assert_eq!(header(&head, "connection"), Some("close"));
            assert!(String::from_utf8_lossy(&got).starts_with(body), "{path}");
        }
        assert!(srv.served("/lib/a.js"));
        srv.forget_page();
        assert!(!srv.served("/lib/a.js"));
    }

    #[test]
    fn refuses_paths_outside_web() {
        let root = Root::new("escape");
        let srv = Server::start(&root.0).unwrap();
        for path in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/lib/../../secret.txt",
            "/.hidden",
            "/nope.html",
            "/..%5csecret.txt",
        ] {
            assert_eq!(request(srv.port(), "GET", path, "").0, 404, "{path}");
        }
        assert_eq!(request(srv.port(), "DELETE", "/cmd", "").0, 405);
    }

    #[test]
    fn command_channel_and_state() {
        let root = Root::new("cmd");
        let srv = Server::start(&root.0).unwrap();
        let p = srv.port();
        let (code, _, body) = request(p, "GET", "/cmd", "");
        assert_eq!(code, 200);
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, json!({"seq": 0}));

        let seq = srv.send("reset", Some("Draft: \nline"));
        assert_eq!(seq, 1);
        let v: Value = serde_json::from_slice(&request(p, "GET", "/cmd", "").2).unwrap();
        assert_eq!(
            v,
            json!({"seq": 1, "cmd": "reset", "text": "Draft: \nline"})
        );
        let seq2 = srv.send("focus-password", None);
        assert_eq!(seq2, 2);
        let v: Value = serde_json::from_slice(&request(p, "GET", "/cmd", "").2).unwrap();
        assert_eq!(v, json!({"seq": 2, "cmd": "focus-password"}));
        assert!(srv.polls() >= 3);

        // Not acknowledged yet: a bounded wait returns the last state (none).
        let t = Instant::now();
        assert_eq!(
            srv.wait_ack(None, seq2, Duration::from_millis(150)),
            Err(None)
        );
        assert!(t.elapsed() < Duration::from_secs(2));

        // The page posts from another thread while the harness waits.
        let poster = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let a = r#"{"editor":"quill","ack":1,"text":"Draft: ","active":"editor"}"#;
            assert_eq!(request(p, "POST", "/state", a).0, 204);
            std::thread::sleep(Duration::from_millis(50));
            let b = r#"{"editor":"quill","ack":2,"text":"Draft: ","active":"password","password_len":15}"#;
            assert_eq!(request(p, "POST", "/state", b).0, 204);
        });
        let s = srv
            .wait_ack(Some("quill"), seq2, Duration::from_secs(10))
            .unwrap();
        poster.join().unwrap();
        assert_eq!(s["active"], "password");
        assert_eq!(s["password_len"], 15);
        assert_eq!(srv.log().len(), 2);
        assert_eq!(srv.log()[1].1["ack"], 2);
        assert_eq!(
            srv.wait_state(None, Duration::from_secs(1), |s| s["ack"] == 2)
                .unwrap()["editor"],
            "quill"
        );
        // A leftover page of another editor does not hide this editor's state.
        let other = r#"{"editor":"lexical","ack":2,"text":"old","active":"other"}"#;
        assert_eq!(request(p, "POST", "/state", other).0, 204);
        assert_eq!(srv.state(None).unwrap()["editor"], "lexical");
        assert_eq!(srv.state(Some("quill")).unwrap()["active"], "password");
        assert!(
            srv.wait_ack(Some("quill"), 2, Duration::from_millis(100))
                .is_ok()
        );
        assert_eq!(
            srv.wait_ack(Some("nope"), 1, Duration::from_millis(50)),
            Err(None)
        );

        // Bad bodies are refused and leave the state alone.
        assert_eq!(request(p, "POST", "/state", "not json").0, 400);
        assert_eq!(request(p, "POST", "/state", "[1]").0, 400);
        assert_eq!(srv.state(None).unwrap()["ack"], 2);

        // A new page sees no stale command, and seq stays monotonic.
        srv.forget_page();
        assert!(srv.state(None).is_none());
        assert!(srv.state(Some("quill")).is_none());
        let v: Value = serde_json::from_slice(&request(p, "GET", "/cmd", "").2).unwrap();
        assert_eq!(v, json!({"seq": 2}));
        assert_eq!(srv.send("focus-editor", None), 3);
    }

    #[test]
    fn follows_one_page_and_drops_stale_posts() {
        let root = Root::new("token");
        let srv = Server::start(&root.0).unwrap();
        let p = srv.port();
        let seq = srv.send("reset", Some("Draft: "));
        // The first page to poll is followed; another page gets no command.
        let v: Value = serde_json::from_slice(&request(p, "GET", "/cmd?token=aaa", "").2).unwrap();
        assert_eq!(v["cmd"], "reset");
        let v: Value = serde_json::from_slice(&request(p, "GET", "/cmd?token=bbb", "").2).unwrap();
        assert_eq!(v, json!({"seq": seq}));
        let post = |body: &str| request(p, "POST", "/state", body).0;
        assert_eq!(
            post(r#"{"editor":"quill","token":"bbb","n":9,"ack":1,"text":"x"}"#),
            204
        );
        assert_eq!(srv.state(None), None, "another page's state is ignored");
        assert_eq!(
            post(r#"{"editor":"quill","token":"aaa","n":2,"ack":1,"text":"new"}"#),
            204
        );
        assert_eq!(
            post(r#"{"editor":"quill","token":"aaa","n":1,"ack":1,"text":"old"}"#),
            204
        );
        assert_eq!(
            srv.state(Some("quill")).unwrap()["text"],
            "new",
            "older post dropped"
        );
        assert_eq!(srv.log().len(), 1);
        // A new launch follows a new page.
        srv.forget_page();
        assert_eq!(
            post(r#"{"editor":"quill","token":"ccc","n":1,"ack":1,"text":"c"}"#),
            204
        );
        assert_eq!(srv.state(None).unwrap()["text"], "c");
    }

    #[test]
    fn concurrent_clients() {
        let root = Root::new("conc");
        let srv = Server::start(&root.0).unwrap();
        let p = srv.port();
        let hs: Vec<_> = (0..16)
            .map(|i| {
                std::thread::spawn(move || {
                    let body = format!(r#"{{"ack":{i},"text":"t{i}"}}"#);
                    assert_eq!(request(p, "POST", "/state", &body).0, 204);
                    assert_eq!(request(p, "GET", "/index.html", "").0, 200);
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(srv.log().len(), 16);
        // Stopping joins the accept thread without hanging.
        drop(srv);
    }

    #[test]
    fn resolve_paths() {
        let base = Path::new("base");
        assert_eq!(resolve(base, "/a/b.js"), Some(base.join("a").join("b.js")));
        assert_eq!(resolve(base, "/"), Some(base.join("index.html")));
        assert_eq!(resolve(base, "/a%20b.js"), Some(base.join("a b.js")));
        assert_eq!(resolve(base, "/a/../b"), None);
        assert_eq!(resolve(base, "/c:/x"), None);
        assert_eq!(resolve(base, "/%zz"), None);
        assert_eq!(resolve(base, "/CON"), None);
        assert_eq!(resolve(base, "/a/nul.txt"), None);
        assert_eq!(resolve(base, "/com1.js"), None);
        assert_eq!(resolve(base, "/LPT9"), None);
        assert!(resolve(base, "/console.js").is_some());
        assert!(resolve(base, "/com10.js").is_some());
        assert_eq!(content_type("/x.mjs"), "text/javascript; charset=utf-8");
    }
}
