//! MCP exit criteria: real MCP clients against a daemon on a temporary home, connected exactly
//! as the configs `nuggetd mcp install --client claude-desktop|claude-code|cursor` produce say
//! (command, args and env for the stdio bridge), and over loopback HTTP as the `generic-http`
//! setup snippet says (url and Authorization header). Each initializes, lists tools and calls
//! `get_context`. Also: the HTTP listener fails closed on a taken port, and no client config
//! ever points at a port the daemon does not hold. Loopback only; no external network.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use nugget_core::ipc::{McpClientCreated, method};
use nugget_core::paths::Paths;
use nugget_ingest::{MemorySecrets, NoPowerPolicy};
use nugget_ipc::Client;
use nugget_store::StoreKey;
use nuggetd::{Daemon, DaemonOptions, Startup};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const EXE: &str = env!("CARGO_BIN_EXE_nuggetd");
const CALL: Duration = Duration::from_secs(30);
const EVENTUALLY: Duration = Duration::from_secs(60);

const NOTE: &str = "# Falcon launch\n\nThe Falcon launch review is on Thursday. Bring the \
                    telemetry dashboard and the rollback checklist.\n";

type McpClient = RunningService<rmcp::RoleClient, ()>;

struct Harness {
    tmp: tempfile::TempDir,
    home: PathBuf,
    client: Client,
    mcp_addr: Option<std::net::SocketAddr>,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<nugget_core::Result<()>>>,
}

impl Harness {
    async fn start(mcp_port: u16) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("nugget-mcpc-")
            .tempdir()
            .unwrap();
        let home = tmp.path().join("home");
        let paths = Paths::rooted(home.clone());
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(
            paths.config_file(),
            format!(
                "[runtime]\nenabled = false\n\n[knowledge]\ninterval_minutes = 0\n\n\
                 [mcp]\nport = {mcp_port}\n"
            ),
        )
        .unwrap();
        let mut opts = DaemonOptions::new(paths.clone());
        opts.store_key = Some(StoreKey::from_bytes([5; 32]));
        opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
        opts.power = Some(Arc::new(NoPowerPolicy));
        opts.home = Some(home.clone());
        opts.nuggetd_exe = Some(PathBuf::from(EXE));
        let daemon = match Daemon::start(opts).await.unwrap() {
            Startup::Ready(d) => d,
            Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
        };
        let mcp_addr = daemon.mcp_http_addr();
        let (tx, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(daemon.run_until(async {
            let _ = rx.await;
        }));
        let client = Client::connect(&paths).await.unwrap();
        Self {
            tmp,
            home,
            client,
            mcp_addr,
            stop: Some(tx),
            server: Some(server),
        }
    }

    async fn call<R: serde::de::DeserializeOwned>(&self, m: &str, params: Value) -> R {
        tokio::time::timeout(CALL, self.client.call(m, &params))
            .await
            .unwrap_or_else(|_| panic!("{m} timed out"))
            .unwrap_or_else(|e| panic!("{m} failed: {e}"))
    }

    async fn add_notes(&self) {
        let root = self.tmp.path().join("notes");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("falcon.md"), NOTE).unwrap();
        let _: Value = self
            .call(
                method::SOURCES_ADD,
                json!({"kind": "files", "name": "notes", "settings": {"roots": [root], "exclude": []}}),
            )
            .await;
    }

    async fn create_client(&self, name: &str) -> McpClientCreated {
        self.call(
            method::MCP_CLIENTS_CREATE,
            json!({"name": name, "scopes": {"sources": null, "tools": [], "include_sensitive": false}}),
        )
        .await
    }

    /// `nuggetd <args>` as a user runs it on this home.
    async fn run_bin(&self, args: &[&str]) -> std::process::Output {
        let out = tokio::process::Command::new(EXE)
            .args(args)
            .env("NUGGET_HOME", &self.home)
            .env("NUGGET_KEYCHAIN", "off")
            .env_remove(nugget_mcp::TOKEN_ENV)
            .stdin(Stdio::null())
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "nuggetd {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    async fn stop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(CALL, self.server.take().unwrap())
            .await
            .expect("daemon stops promptly")
            .unwrap()
            .unwrap();
    }
}

/// A stdio server entry as an app reads it from its config.
#[derive(Debug)]
struct StdioEntry {
    command: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl StdioEntry {
    /// The `mcpServers.nugget` entry of a Claude Desktop / Cursor JSON config.
    fn from_json(config: &Value) -> Self {
        let e = &config["mcpServers"]["nugget"];
        let strs = |v: &Value| -> Vec<String> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_owned())
                .collect()
        };
        Self {
            command: e["command"].as_str().unwrap().to_owned(),
            args: strs(&e["args"]),
            env: e["env"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                .collect(),
        }
    }

    /// `claude mcp add --transport stdio --scope user --env K=V ... nugget -- <command> <args>`.
    fn from_claude_code(command_line: &str) -> Self {
        let words = shell_words(command_line);
        assert_eq!(&words[..3], ["claude", "mcp", "add"], "{command_line}");
        let dash = words
            .iter()
            .position(|w| w == "--")
            .expect("`--` separator");
        let mut env = Vec::new();
        let mut i = 3;
        while i < dash {
            match words[i].as_str() {
                "--env" | "-e" => {
                    let (k, v) = words[i + 1].split_once('=').unwrap();
                    env.push((k.to_owned(), v.to_owned()));
                    i += 2;
                }
                "--transport" | "--scope" => {
                    i += 2;
                }
                name => {
                    assert_eq!(name, "nugget", "server name");
                    i += 1;
                }
            }
        }
        Self {
            command: words[dash + 1].clone(),
            args: words[dash + 2..].to_vec(),
            env,
        }
    }

    /// Launch it as the app would (no inherited Nugget variables), and connect over its stdio.
    async fn connect(&self) -> (McpClient, tokio::process::Child) {
        let mut cmd = tokio::process::Command::new(&self.command);
        cmd.args(&self.args)
            .env_remove("NUGGET_HOME")
            .env_remove(nugget_mcp::TOKEN_ENV)
            .env("NUGGET_KEYCHAIN", "off")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();
        let client = tokio::time::timeout(CALL, ().serve((stdout, stdin)))
            .await
            .expect("bridge initializes in time")
            .expect("bridge initializes");
        (client, child)
    }
}

/// POSIX-shell word splitting for what the snippets emit (plain words and single quotes).
fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.trim().chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

/// Initialize (done by the caller's connect), list tools, and call `get_context` until the note
/// is indexed. Returns the context text.
async fn exercise(client: &McpClient, what: &str) -> String {
    let info = client.peer_info().expect("initialized");
    assert!(
        info.server_info
            .as_ref()
            .is_some_and(|s| !s.name.is_empty()),
        "{what}: server info"
    );
    let tools = client.list_all_tools().await.unwrap();
    for t in ["search", "get_context", "get_entity"] {
        assert!(tools.iter().any(|x| x.name == t), "{what}: {t} listed");
    }
    let deadline = tokio::time::Instant::now() + EVENTUALLY;
    loop {
        let res = client
            .call_tool(
                CallToolRequestParams::new("get_context").with_arguments(
                    json!({"query": "Falcon launch review telemetry"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap_or_else(|e| panic!("{what}: get_context: {e}"));
        assert_ne!(res.is_error, Some(true), "{what}: {res:?}");
        let text: String = res
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect();
        if text.contains("rollback checklist") {
            return text;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: note never reached get_context: {text}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn finish(what: &str, client: McpClient, mut child: tokio::process::Child) {
    client.cancel().await.unwrap();
    let status = tokio::time::timeout(CALL, child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{what}: bridge exits cleanly");
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// `url:` and `header:` lines of the `generic-http` snippet.
fn http_snippet(created: &McpClientCreated) -> Option<(String, String, String)> {
    let s = created.setup.iter().find(|s| s.app == "generic-http")?;
    let mut url = None;
    let mut header = None;
    for line in s.text.lines() {
        if let Some(u) = line.strip_prefix("url: ") {
            url = Some(u.trim().to_owned());
        } else if let Some(h) = line.strip_prefix("header: ") {
            let (k, v) = h.split_once(':').unwrap();
            header = Some((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    let (k, v) = header.expect("header line");
    Some((url.expect("url line"), k, v))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn installed_app_configs_reach_the_daemon_over_stdio_and_http() {
    let mut h = Harness::start(0).await;
    let addr = h.mcp_addr.expect("http listener on a free loopback port");
    h.add_notes().await;

    // Claude Desktop and Cursor: the merged JSON config, launched as written.
    for (app, file) in [
        ("claude-desktop", "claude_desktop_config.json"),
        ("cursor", "mcp.json"),
    ] {
        let cfg = h.tmp.path().join(app).join(file);
        let cfg_s = cfg.display().to_string();
        h.run_bin(&[
            "mcp", "install", "--client", app, "--config", &cfg_s, "--yes",
        ])
        .await;
        let entry = StdioEntry::from_json(&read_json(&cfg));
        assert_eq!(entry.command, EXE, "{app}");
        assert_eq!(&entry.args[..2], ["mcp", "--client"], "{app}");
        assert!(
            entry
                .env
                .iter()
                .any(|(k, v)| k == "NUGGET_HOME" && Path::new(v) == h.home.as_path()),
            "{app}: {entry:?}"
        );
        let (client, child) = entry.connect().await;
        exercise(&client, app).await;
        finish(app, client, child).await;
    }

    // Claude Code: the printed `claude mcp add` command, parsed as a shell would.
    let out = h
        .run_bin(&["mcp", "install", "--client", "claude-code", "--yes"])
        .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("claude mcp add"))
        .unwrap_or_else(|| panic!("no command in {stdout}"));
    let entry = StdioEntry::from_claude_code(line);
    assert_eq!(entry.command, EXE);
    let (client, child) = entry.connect().await;
    exercise(&client, "claude-code").await;
    finish("claude-code", client, child).await;

    // HTTP: the generic-http snippet's url and header, on the port the daemon holds.
    let created = h.create_client("HTTP app").await;
    let (url, header, value) = http_snippet(&created).expect("http snippet while listening");
    assert_eq!(url, format!("http://127.0.0.1:{}/mcp", addr.port()));
    assert_eq!(header, "Authorization");
    let token = value.strip_prefix("Bearer ").expect("bearer token");
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token.to_owned()),
    );
    let client = tokio::time::timeout(CALL, ().serve(transport))
        .await
        .unwrap()
        .expect("http client initializes");
    exercise(&client, "http").await;
    client.cancel().await.unwrap();
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_taken_port_is_never_offered_to_clients() {
    // Another program holds the configured port: the daemon does not listen elsewhere, offers no
    // HTTP setup, and stdio clients still work.
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = squatter.local_addr().unwrap().port();
    let mut h = Harness::start(port).await;
    assert_eq!(h.mcp_addr, None, "no listener on a taken port");
    let created = h.create_client("Desktop").await;
    assert!(
        created.setup.iter().all(|s| s.app != "generic-http"),
        "HTTP setup offered for a port the daemon does not hold: {:?}",
        created.setup.iter().map(|s| &s.app).collect::<Vec<_>>()
    );
    assert!(
        created
            .setup
            .iter()
            .all(|s| !s.text.contains(&format!(":{port}"))),
        "a snippet points at the squatted port"
    );
    h.add_notes().await;
    let desktop = created
        .setup
        .iter()
        .find(|s| s.app == "claude-desktop")
        .unwrap();
    let entry = StdioEntry::from_json(&serde_json::from_str(&desktop.text).unwrap());
    let (client, child) = entry.connect().await;
    exercise(&client, "stdio with http down").await;
    finish("stdio with http down", client, child).await;
    drop(squatter);
    h.stop().await;
}

#[test]
fn shell_words_split_like_a_shell() {
    assert_eq!(
        shell_words("a --env K='/x y' 'it'\\''s' b\\ c"),
        ["a", "--env", "K=/x y", "it's", "b c"]
    );
}
