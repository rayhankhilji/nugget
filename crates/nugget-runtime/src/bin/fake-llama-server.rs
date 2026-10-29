//! A stand-in for llama.cpp's `llama-server`, for tests (built only with the `fake-server`
//! feature).
//!
//! It accepts the arguments `nugget-runtime` passes (and rejects anything else, like the real
//! server), listens on `--host`/`--port` (or, on Unix, on the socket `--host <path>.sock`, like
//! llama-server), requires the `LLAMA_API_KEY` bearer token on every
//! endpoint except `/health`, and serves:
//! - `GET /health`: 503 while "loading" (see `--fake-start-delay-ms`), then `{"status":"ok"}`;
//! - `GET /v1/models`;
//! - `POST /v1/chat/completions` (JSON or SSE stream; refused with `--embedding`);
//! - `POST /v1/embeddings` (deterministic 8-dim unit vectors; needs `--embedding`);
//! - `GET /metrics` (needs `--metrics`);
//! - `GET /fake/info`: argv and environment variable *names*; `POST /fake/crash`: exit 42.
//!
//! Test controls (extra arguments):
//! - `--fake-start-delay-ms N`: report "loading" for N ms;
//! - `--fake-crash-after-ms N`: exit with code 3 N ms after the first healthy `/health` answer
//!   (so the crash always comes after the supervisor saw the server ready, however slow the host);
//! - `--fake-crash-once PATH`: with `--fake-crash-after-ms`, crash only if PATH does not exist
//!   (and create it), so the restarted instance stays up;
//! - `--fake-exit-immediately CODE`: exit at once (start-up failure);
//! - `--fake-spawn-child PIDFILE`: start a sleeping child process and write its pid (process
//!   group cleanup tests);
//! - `--fake-print-key`: print the API key to stderr (log redaction tests);
//! - `--fake-ignore-sigterm`: survive SIGTERM (Unix), so a stop takes the whole grace period;
//! - `--fake-squat PIDFILE`: do not listen; instead start a *separate* process (own process
//!   group, so not one of ours) that listens on `--port`, and write its pid. Simulates another
//!   local user grabbing the port before the server binds it;
//! - `--fake-squatter LOGFILE`: be that process: answer `/health` with ok on `--port` and append
//!   every request line (and whether it carried an `authorization` header) to LOGFILE; exits
//!   after 60 s;
//! - `--fake-sleeper`: just sleep (used for the spawned child).

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

/// Set once `/health` has answered 200; `--fake-crash-after-ms` counts from then.
static SERVED_HEALTHY: AtomicBool = AtomicBool::new(false);

/// A connection, over TCP or a Unix socket.
trait Conn: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Conn for T {}

#[derive(Debug, Default)]
struct Opts {
    host: String,
    port: u16,
    alias: String,
    model: String,
    embedding: bool,
    metrics: bool,
    start_delay: Duration,
    crash_after: Option<Duration>,
    crash_once: Option<String>,
    exit_immediately: Option<u8>,
    spawn_child: Option<String>,
    print_key: bool,
    sleeper: bool,
    ignore_sigterm: bool,
    squat: Option<String>,
    squatter: Option<String>,
}

const FLAGS_WITH_VALUE: &[&str] = &[
    "--host",
    "--port",
    "--alias",
    "--model",
    "--ctx-size",
    "--parallel",
    "--n-gpu-layers",
    "--threads",
    "--pooling",
    "--batch-size",
    "--ubatch-size",
    "--fake-start-delay-ms",
    "--fake-crash-after-ms",
    "--fake-crash-once",
    "--fake-exit-immediately",
    "--fake-spawn-child",
    "--fake-squat",
    "--fake-squatter",
];
const SWITCHES: &[&str] = &[
    "--embedding",
    "--no-webui",
    "--offline",
    "--no-slots",
    "--metrics",
    "--fake-print-key",
    "--fake-sleeper",
    "--fake-ignore-sigterm",
];

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        host: "127.0.0.1".into(),
        port: 8080,
        ..Opts::default()
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if SWITCHES.contains(&a.as_str()) {
            match a.as_str() {
                "--embedding" => o.embedding = true,
                "--metrics" => o.metrics = true,
                "--fake-print-key" => o.print_key = true,
                "--fake-sleeper" => o.sleeper = true,
                "--fake-ignore-sigterm" => o.ignore_sigterm = true,
                _ => {}
            }
            continue;
        }
        if !FLAGS_WITH_VALUE.contains(&a.as_str()) {
            return Err(format!("error: invalid argument: {a}"));
        }
        let v = it
            .next()
            .ok_or(format!("error: {a} needs a value"))?
            .clone();
        let num = |v: &str| {
            v.parse::<u64>()
                .map_err(|_| format!("error: bad number {v}"))
        };
        match a.as_str() {
            "--host" => o.host = v,
            "--port" => o.port = num(&v)? as u16,
            "--alias" => o.alias = v,
            "--model" => o.model = v,
            "--fake-start-delay-ms" => o.start_delay = Duration::from_millis(num(&v)?),
            "--fake-crash-after-ms" => o.crash_after = Some(Duration::from_millis(num(&v)?)),
            "--fake-crash-once" => o.crash_once = Some(v),
            "--fake-exit-immediately" => o.exit_immediately = Some(num(&v)? as u8),
            "--fake-spawn-child" => o.spawn_child = Some(v),
            "--fake-squat" => o.squat = Some(v),
            "--fake-squatter" => o.squatter = Some(v),
            "--ctx-size" | "--parallel" | "--threads" | "--batch-size" | "--ubatch-size" => {
                num(&v)?;
            }
            "--n-gpu-layers" if v != "auto" && v != "all" => {
                num(&v)?;
            }
            "--pooling" if !["none", "mean", "cls", "last", "rank"].contains(&v.as_str()) => {
                return Err(format!("error: bad pooling {v}"));
            }
            _ => {}
        }
    }
    Ok(o)
}

struct Server {
    opts: Opts,
    key: Option<String>,
    started: Instant,
    prompt_tokens: AtomicU64,
    predicted: AtomicU64,
    argv: Vec<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse(&argv) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if opts.sleeper {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        return ExitCode::SUCCESS;
    }
    if let Some(log) = &opts.squatter {
        return squatter(opts.port, log).await;
    }
    if let Some(code) = opts.exit_immediately {
        eprintln!("fake: exiting immediately with {code}");
        return ExitCode::from(code);
    }
    let unix = opts.host.ends_with(".sock");
    if opts.host != "127.0.0.1" && !(unix && cfg!(unix)) {
        eprintln!("fake: refusing non-loopback host {}", opts.host);
        return ExitCode::from(3);
    }
    if opts.model.is_empty() || !std::path::Path::new(&opts.model).is_file() {
        eprintln!("fake: failed to load model '{}'", opts.model);
        return ExitCode::from(1);
    }
    let key = std::env::var("LLAMA_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    if opts.print_key {
        eprintln!("fake: api key is {}", key.as_deref().unwrap_or("<none>"));
    }
    if let Some(pidfile) = &opts.spawn_child {
        let me = std::env::current_exe().expect("own path");
        let child = std::process::Command::new(me)
            .arg("--fake-sleeper")
            .spawn()
            .expect("spawn sleeper");
        std::fs::write(pidfile, child.id().to_string()).expect("write pid file");
        // Deliberately not waited for: it must die with the process group.
        std::mem::forget(child);
    }
    #[cfg(not(unix))]
    let _ = opts.ignore_sigterm;
    #[cfg(unix)]
    if opts.ignore_sigterm {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::spawn(async move {
            while term.recv().await.is_some() {
                eprintln!("fake: ignoring SIGTERM");
            }
        });
    }
    if let Some(pidfile) = &opts.squat {
        let me = std::env::current_exe().expect("own path");
        let log = format!("{pidfile}.log");
        let mut cmd = std::process::Command::new(me);
        cmd.args(["--fake-squatter", &log, "--port", &opts.port.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Not in our process group: it stands for somebody else's process.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let child = cmd.spawn().expect("spawn squatter");
        std::fs::write(pidfile, child.id().to_string()).expect("write pid file");
        std::mem::forget(child);
        // Stay alive (like a server still loading its model), but never listen.
        tokio::time::sleep(Duration::from_secs(3600)).await;
        return ExitCode::SUCCESS;
    }
    if let Some(after) = opts.crash_after {
        let armed = match &opts.crash_once {
            Some(marker) if std::path::Path::new(marker).exists() => false,
            Some(marker) => {
                let _ = std::fs::write(marker, b"crashed");
                true
            }
            None => true,
        };
        if armed {
            tokio::spawn(async move {
                while !SERVED_HEALTHY.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                tokio::time::sleep(after).await;
                eprintln!("fake: simulated crash");
                std::process::exit(3);
            });
        }
    }
    let listener = match Listener::bind(&opts.host, opts.port).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("fake: couldn't bind HTTP server socket: {e}");
            return ExitCode::from(1);
        }
    };
    if unix {
        println!("fake: listening on {}", opts.host);
    } else {
        println!("fake: listening on {}:{}", opts.host, opts.port);
    }
    eprintln!("main: model loaded (alias {})", opts.alias);
    let srv = Arc::new(Server {
        opts,
        key,
        started: Instant::now(),
        prompt_tokens: AtomicU64::new(0),
        predicted: AtomicU64::new(0),
        argv,
    });
    loop {
        let Ok(conn) = listener.accept().await else {
            continue;
        };
        let srv = srv.clone();
        tokio::spawn(async move {
            let _ = handle(conn, &srv).await;
        });
    }
}

enum Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
}

impl Listener {
    async fn bind(host: &str, port: u16) -> std::io::Result<Self> {
        #[cfg(unix)]
        if host.ends_with(".sock") {
            return tokio::net::UnixListener::bind(host).map(Listener::Unix);
        }
        Ok(Listener::Tcp(TcpListener::bind((host, port)).await?))
    }

    async fn accept(&self) -> std::io::Result<Box<dyn Conn>> {
        match self {
            Listener::Tcp(l) => Ok(Box::new(l.accept().await?.0)),
            #[cfg(unix)]
            Listener::Unix(l) => Ok(Box::new(l.accept().await?.0)),
        }
    }
}

/// `--fake-squatter`: somebody else's server on our port.
async fn squatter(port: u16, log: &str) -> ExitCode {
    let Ok(listener) = TcpListener::bind(("127.0.0.1", port)).await else {
        return ExitCode::from(1);
    };
    let log = log.to_string();
    let serve = async move {
        loop {
            let Ok((mut conn, _)) = listener.accept().await else {
                continue;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let Ok(Some(req)) = read_request(&mut conn).await else {
                    return;
                };
                let auth = req.headers.contains_key("authorization");
                let line = format!("{} {} authorization={auth}\n", req.method, req.path);
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log)
                {
                    use std::io::Write;
                    let _ = f.write_all(line.as_bytes());
                }
                let _ = respond(&mut conn, 200, &json!({"status": "ok"})).await;
            });
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(60), serve).await;
    ExitCode::SUCCESS
}

struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

async fn read_request(conn: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Option<Request>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > 64 * 1024 {
            return Ok(None);
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default().to_string();
    let path = first.next().unwrap_or_default().to_string();
    let headers: BTreeMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
        .min(4 * 1024 * 1024);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok(Some(Request {
        method,
        path,
        headers,
        body,
    }))
}

async fn respond(
    conn: &mut (impl AsyncWrite + Unpin),
    status: u16,
    body: &Value,
) -> std::io::Result<()> {
    let text = body.to_string();
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        text.len()
    );
    conn.write_all(head.as_bytes()).await?;
    conn.write_all(text.as_bytes()).await?;
    conn.shutdown().await
}

fn error(code: u16, msg: &str) -> (u16, Value) {
    (code, json!({"error": {"code": code, "message": msg}}))
}

async fn handle(mut conn: Box<dyn Conn>, srv: &Server) -> std::io::Result<()> {
    let Some(req) = read_request(&mut conn).await? else {
        return Ok(());
    };
    let loading = srv.started.elapsed() < srv.opts.start_delay;
    if req.path == "/health" || req.path == "/v1/health" {
        let (code, body) = if loading {
            error(503, "Loading model")
        } else {
            SERVED_HEALTHY.store(true, Ordering::SeqCst);
            (200, json!({"status": "ok"}))
        };
        return respond(&mut conn, code, &body).await;
    }
    if let Some(key) = &srv.key {
        let ok = req
            .headers
            .get("authorization")
            .is_some_and(|v| v == &format!("Bearer {key}"));
        if !ok {
            let (c, b) = error(401, "Invalid API Key");
            return respond(&mut conn, c, &b).await;
        }
    }
    if loading {
        let (c, b) = error(503, "Loading model");
        return respond(&mut conn, c, &b).await;
    }
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/v1/models") | ("GET", "/models") => {
            let b = json!({"object": "list", "data": [{"id": srv.opts.alias, "object": "model", "owned_by": "llamacpp"}]});
            respond(&mut conn, 200, &b).await
        }
        ("POST", "/v1/embeddings") => {
            if !srv.opts.embedding {
                let (c, b) = error(
                    501,
                    "This server does not support embeddings. Start it with `--embeddings`",
                );
                return respond(&mut conn, c, &b).await;
            }
            let inputs: Vec<String> = match body.get("input") {
                Some(Value::String(s)) => vec![s.clone()],
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
                _ => {
                    let (c, b) = error(400, "\"input\" must be a string or an array of strings");
                    return respond(&mut conn, c, &b).await;
                }
            };
            let tokens: u64 = inputs
                .iter()
                .map(|s| s.split_whitespace().count() as u64 + 1)
                .sum();
            srv.prompt_tokens.fetch_add(tokens, Ordering::Relaxed);
            let data: Vec<Value> = inputs
                .iter()
                .enumerate()
                .map(|(i, s)| json!({"object": "embedding", "index": i, "embedding": embed(s)}))
                .collect();
            let b = json!({"object": "list", "model": srv.opts.alias, "data": data,
                "usage": {"prompt_tokens": tokens, "total_tokens": tokens}});
            respond(&mut conn, 200, &b).await
        }
        ("POST", "/v1/chat/completions") => {
            if srv.opts.embedding {
                let (c, b) = error(
                    501,
                    "This server does not support completions. Start it without `--embeddings`",
                );
                return respond(&mut conn, c, &b).await;
            }
            srv.prompt_tokens.fetch_add(5, Ordering::Relaxed);
            srv.predicted.fetch_add(2, Ordering::Relaxed);
            let alias = &srv.opts.alias;
            if body.get("stream").and_then(Value::as_bool) == Some(true) {
                let chunks = [
                    json!({"id": "c1", "object": "chat.completion.chunk", "model": alias, "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hello"}}]}),
                    json!({"id": "c1", "object": "chat.completion.chunk", "model": alias, "choices": [{"index": 0, "delta": {"content": " from fake"}}]}),
                    json!({"id": "c1", "object": "chat.completion.chunk", "model": alias, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
                    json!({"id": "c1", "object": "chat.completion.chunk", "model": alias, "choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}}),
                ];
                let mut out = String::new();
                for c in chunks {
                    out.push_str(&format!("data: {c}\n\n"));
                }
                out.push_str("data: [DONE]\n\n");
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    out.len()
                );
                conn.write_all(head.as_bytes()).await?;
                conn.write_all(out.as_bytes()).await?;
                return conn.shutdown().await;
            }
            let b = json!({"id": "c1", "object": "chat.completion", "model": alias,
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hello from fake"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}});
            respond(&mut conn, 200, &b).await
        }
        ("GET", "/metrics") => {
            if !srv.opts.metrics {
                let (c, b) = error(
                    501,
                    "This server does not support metrics endpoint. Start it with `--metrics`",
                );
                return respond(&mut conn, c, &b).await;
            }
            let text = format!(
                "# HELP llamacpp:prompt_tokens_total Number of prompt tokens processed.\n\
                 # TYPE llamacpp:prompt_tokens_total counter\n\
                 llamacpp:prompt_tokens_total {}\n\
                 llamacpp:tokens_predicted_total {}\n\
                 llamacpp:requests_processing 0\n\
                 llamacpp:requests_deferred 0\n",
                srv.prompt_tokens.load(Ordering::Relaxed),
                srv.predicted.load(Ordering::Relaxed)
            );
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain; version=0.0.4\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                text.len()
            );
            conn.write_all(head.as_bytes()).await?;
            conn.write_all(text.as_bytes()).await?;
            conn.shutdown().await
        }
        ("GET", "/fake/info") => {
            let env: Vec<String> = std::env::vars_os()
                .map(|(k, _)| k.to_string_lossy().into_owned())
                .collect();
            let b = json!({"argv": srv.argv, "env": env, "pid": std::process::id()});
            respond(&mut conn, 200, &b).await
        }
        ("POST", "/fake/crash") => {
            eprintln!("fake: crash requested");
            std::process::exit(42);
        }
        _ => {
            let (c, b) = error(404, "File Not Found");
            respond(&mut conn, c, &b).await
        }
    }
}

/// Deterministic unit vector from the text.
fn embed(s: &str) -> Vec<f32> {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    let v: Vec<f32> = (0..8)
        .map(|i| {
            let x = h.rotate_left(i * 8) & 0xffff;
            x as f32 / 65535.0 - 0.5
        })
        .collect();
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.into_iter().map(|x| x / n).collect()
}
