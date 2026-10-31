//! The MCP setup guide (`docs/book/src/guide/mcp.md`) shows the same snippets the daemon hands
//! out (`setup_snippets`), and every hand-written host config in it launches the bridge or reaches
//! the HTTP endpoint the same way. The compat clients (`crates/nugget-mcp/compat`) connect with
//! the daemon's snippets, so CI exercises what the guide tells users to paste.

use std::path::PathBuf;

use nugget_core::ipc::McpClientInfo;
use nugget_mcp::{SnippetOptions, TOKEN_ENV, setup_snippets};
use serde_json::Value;

/// Placeholders the guide uses for the client id and token.
const CLIENT_ID: &str = "mcp_YOUR_CLIENT_ID";
const TOKEN: &str = "ngt_YOUR_TOKEN";
const HTTP_URL: &str = "http://127.0.0.1:7319/mcp";

fn guide() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/book/src/guide/mcp.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn placeholder_client() -> McpClientInfo {
    McpClientInfo {
        id: CLIENT_ID.into(),
        name: "Claude Desktop".into(),
        scopes: nugget_core::ipc::McpScopes {
            sources: None,
            tools: Vec::new(),
            include_sensitive: false,
        },
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        last_used: None,
    }
}

/// Fenced code blocks of `lang`.
fn blocks(doc: &str, lang: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in doc.lines() {
        match &mut cur {
            None if line.trim_start() == format!("```{lang}") => cur = Some(String::new()),
            Some(b) if line.trim_start() == "```" => {
                out.push(std::mem::take(b));
                cur = None;
            }
            Some(b) => {
                b.push_str(line);
                b.push('\n');
            }
            None => {}
        }
    }
    out
}

#[test]
fn the_guide_shows_the_snippets_the_daemon_generates() {
    let doc = guide();
    for os in ["macos", "windows", "linux"] {
        let opts = SnippetOptions {
            nuggetd: "nuggetd".into(),
            http_port: Some(7319),
            os: os.into(),
            env: Vec::new(),
        };
        for s in setup_snippets(&placeholder_client(), TOKEN, &opts) {
            assert!(
                doc.contains(s.text.trim()),
                "the guide lacks the {} snippet exactly as the daemon generates it:\n{}",
                s.app,
                s.text
            );
            if let Some(path) = s.config_path {
                assert!(doc.contains(&path), "the guide lacks {} for {os}", path);
            }
        }
    }
}

/// Every object with a `command` launches `nuggetd mcp --client <id>` with the token in the
/// environment; every object with a `url` is the loopback endpoint with a bearer header.
fn check_server_entries(v: &Value, found: &mut (usize, usize)) {
    match v {
        Value::Object(o) => {
            if let Some(cmd) = o.get("command") {
                let cmd = cmd.as_str().expect("command is a string");
                assert!(cmd.contains("nuggetd"), "command {cmd}");
                let args: Vec<_> = o["args"].as_array().expect("args").iter().collect();
                assert_eq!(args.len(), 3, "args {args:?}");
                assert_eq!(args[0], "mcp");
                assert_eq!(args[1], "--client");
                assert!(
                    o["env"].get(TOKEN_ENV).is_some(),
                    "{TOKEN_ENV} is passed in env"
                );
                found.0 += 1;
            }
            if let Some(url) = o.get("url") {
                assert_eq!(url, HTTP_URL);
                let auth = o["headers"]["Authorization"]
                    .as_str()
                    .expect("Authorization header");
                assert!(auth.starts_with("Bearer "), "{auth}");
                found.1 += 1;
            }
            o.values().for_each(|v| check_server_entries(v, found));
        }
        Value::Array(a) => a.iter().for_each(|v| check_server_entries(v, found)),
        _ => {}
    }
}

#[test]
fn every_host_config_in_the_guide_has_the_tested_shape() {
    let doc = guide();
    let mut found = (0, 0);
    for b in blocks(&doc, "json") {
        let v: Value =
            serde_json::from_str(&b).unwrap_or_else(|e| panic!("invalid JSON block ({e}):\n{b}"));
        check_server_entries(&v, &mut found);
    }
    assert!(found.0 >= 3, "stdio host configs: {}", found.0);
    assert!(found.1 >= 2, "HTTP host configs: {}", found.1);
    for b in blocks(&doc, "sh") {
        for line in b.lines().filter(|l| l.starts_with("claude mcp add")) {
            if line.contains("--transport http") {
                assert!(line.contains(HTTP_URL), "{line}");
                assert!(line.contains("Authorization: Bearer "), "{line}");
            } else {
                assert!(line.contains(&format!("--env {TOKEN_ENV}=")), "{line}");
                assert!(line.contains("mcp --client "), "{line}");
            }
        }
    }
}
