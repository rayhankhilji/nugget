//! MCP exit criteria: real MCP clients against a daemon on a temporary home, connected exactly
//! as the configs `nuggetd mcp install --client claude-desktop|claude-code|cursor` produce say
//! (command, args and env for the stdio bridge), and over loopback HTTP as the `generic-http`
//! setup snippet says (url and Authorization header). Each initializes, lists tools and calls
//! `get_context`. Also: the bridge refuses bad tokens, `mcp install` merges into existing
//! configs, and the HTTP listener fails closed on a taken port. Loopback only.

mod common;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use common::*;
use nugget_core::ipc::{McpClientCreated, McpClientInfo, method};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};

const NOTE: &str = "# Falcon launch\n\nThe Falcon launch review is on Thursday. Bring the \
                    telemetry dashboard and the rollback checklist.\n";

/// No local model: let unflagged items count as not sensitive (GG5-2 opt-out), or the
/// default-scoped clients here would see nothing.
async fn start(mcp_port: u16) -> Harness {
    let config = format!(
        "{NO_RUNTIME}\n[ingest]\nassume_unflagged_safe = true\n\n\
         [knowledge]\ninterval_minutes = 0\n\n[mcp]\nport = {mcp_port}\n"
    );
    let h = Harness::start_with(&config, mcp_opts).await;
    h.add_files("notes", &[("falcon.md", NOTE)]).await;
    h
}

fn home(h: &Harness) -> &Path {
    h.paths.data.parent().unwrap()
}

async fn create_client(h: &Harness, name: &str) -> McpClientCreated {
    h.call(
        method::MCP_CLIENTS_CREATE,
        json!({"name": name, "scopes": all_scopes()}),
    )
    .await
}

/// `nuggetd <args>` as a user runs it on this home.
async fn run_bin(h: &Harness, args: &[&str]) -> std::process::Output {
    tokio::process::Command::new(EXE)
        .args(args)
        .env("NUGGET_HOME", home(h))
        .env("NUGGET_KEYCHAIN", "off")
        .env_remove(nugget_mcp::TOKEN_ENV)
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap()
}

/// [`run_bin`], which must succeed; its stdout.
async fn run_ok(h: &Harness, args: &[&str]) -> String {
    let out = run_bin(h, args).await;
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "nuggetd {args:?}: {err}");
    String::from_utf8_lossy(&out.stdout).into_owned()
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
    let mut h = start(0).await;
    let addr = h.mcp_addr.expect("http listener on a free loopback port");

    // Claude Desktop (merged into an existing config) and Cursor: launched as written.
    let desktop = h
        .tmp
        .path()
        .join("claude-desktop")
        .join("claude_desktop_config.json");
    std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
    let existing = r#"{"theme": "dark", "mcpServers": {"weather": {"command": "weather-mcp"}}}"#;
    std::fs::write(&desktop, existing).unwrap();
    let desktop_s = desktop.display().to_string();
    // Without a terminal, --yes is required.
    let install = [
        "mcp",
        "install",
        "--client",
        "claude-desktop",
        "--config",
        &desktop_s,
    ];
    let out = run_bin(&h, &install).await;
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("--yes"));
    for (app, file) in [
        ("claude-desktop", "claude_desktop_config.json"),
        ("cursor", "mcp.json"),
    ] {
        let cfg = h.tmp.path().join(app).join(file);
        let cfg_s = cfg.display().to_string();
        let stdout = run_ok(
            &h,
            &[
                "mcp", "install", "--client", app, "--config", &cfg_s, "--yes",
            ],
        )
        .await;
        assert!(
            !stdout.contains("ngt_"),
            "the token is not printed: {stdout}"
        );
        let entry = StdioEntry::from_json(&read_json(&cfg));
        assert_eq!(entry.command, EXE, "{app}");
        assert_eq!(&entry.args[..2], ["mcp", "--client"], "{app}");
        assert!(
            entry
                .env
                .iter()
                .any(|(k, v)| k == "NUGGET_HOME" && Path::new(v) == home(&h)),
            "{app}: {entry:?}"
        );
        let (client, child) = entry.connect().await;
        exercise(&client, app).await;
        finish(app, client, child).await;
    }
    let v = read_json(&desktop);
    assert_eq!(v["theme"], "dark");
    assert_eq!(v["mcpServers"]["weather"]["command"], "weather-mcp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&desktop).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let backups = std::fs::read_dir(desktop.parent().unwrap())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("nugget-backup")
        });
    assert_eq!(backups.count(), 1);

    // Installing again replaces the entry and revokes the client it replaced.
    let first = StdioEntry::from_json(&v).args[2].clone();
    let stdout = run_ok(&h, &[&install[..], &["--yes"]].concat()).await;
    assert!(stdout.contains(&first), "{stdout}");
    let listed: Vec<McpClientInfo> = h.call(method::MCP_CLIENTS_LIST, json!({})).await;
    assert!(listed.iter().all(|c| c.id != first));
    assert!(!serde_json::to_string(&listed).unwrap().contains("ngt_"));

    // A wrong token (or none) is refused before any MCP traffic.
    let entry = StdioEntry::from_json(&read_json(&desktop));
    let mut cmd = tokio::process::Command::new(EXE);
    cmd.args(&entry.args)
        .env("NUGGET_HOME", home(&h))
        .stdin(Stdio::null());
    let out = cmd
        .env(nugget_mcp::TOKEN_ENV, format!("ngt_{}", "A".repeat(43)))
        .output()
        .await
        .unwrap();
    assert!(
        !out.status.success() && out.stdout.is_empty(),
        "nothing but MCP on stdout"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("revoked"));
    let args: Vec<&str> = entry.args.iter().map(String::as_str).collect();
    let out = run_bin(&h, &args).await;
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(nugget_mcp::TOKEN_ENV));

    // Claude Code: the printed `claude mcp add` command, parsed as a shell would.
    let stdout = run_ok(&h, &["mcp", "install", "--client", "claude-code", "--yes"]).await;
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("claude mcp add"))
        .unwrap_or_else(|| panic!("no command in {stdout}"));
    let entry = StdioEntry::from_claude_code(line);
    assert_eq!(entry.command, EXE);
    let (client, child) = entry.connect().await;
    exercise(&client, "claude-code").await;
    finish("claude-code", client, child).await;
    // Stdio tool calls are audited (by a background writer, so possibly just after the exit).
    eventually("stdio tool calls are audited", || async {
        h.audit()
            .iter()
            .any(|a| a.0 == "mcp.tool_call" && a.2["transport"] == "stdio")
    })
    .await;

    // HTTP: the generic-http snippet's url and header, on the port the daemon holds.
    let created = create_client(&h, "HTTP app").await;
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
    let mut h = start(port).await;
    assert_eq!(h.mcp_addr, None, "no listener on a taken port");
    let created = create_client(&h, "Desktop").await;
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
