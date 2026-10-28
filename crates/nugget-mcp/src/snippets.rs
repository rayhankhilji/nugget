//! Copy-paste setup for MCP apps.
//!
//! Stdio apps (Claude Desktop, Claude Code, Cursor) launch `nuggetd mcp --client <id>` and pass
//! the token in the [`TOKEN_ENV`] environment variable. HTTP clients send it in the
//! `Authorization` header. The token never appears in a file path or a URL.

use nugget_core::ipc::{McpClientCreated, McpClientInfo, McpSetupSnippet};
use serde_json::json;

use crate::auth::ClientToken;
use crate::http::MCP_PATH;
use crate::stdio::TOKEN_ENV;

/// Name the server is registered under in each app.
pub const SERVER_KEY: &str = "nugget";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetOptions {
    /// Path (or bare name on `PATH`) of the `nuggetd` binary.
    pub nuggetd: String,
    /// Loopback HTTP port, when the HTTP listener is enabled.
    pub http_port: Option<u16>,
    /// `std::env::consts::OS` of the machine the apps run on (picks config paths).
    pub os: String,
    /// Extra environment variables the launched `nuggetd mcp` process gets, in order, ahead of
    /// [`TOKEN_ENV`] (e.g. `NUGGET_HOME` when the daemon runs on a non-default data root). A
    /// pair named [`TOKEN_ENV`] is ignored: the token always comes from the client.
    pub env: Vec<(String, String)>,
}

impl Default for SnippetOptions {
    fn default() -> Self {
        Self {
            nuggetd: "nuggetd".into(),
            http_port: None,
            os: std::env::consts::OS.into(),
            env: Vec::new(),
        }
    }
}

fn claude_desktop_config_path(os: &str) -> &'static str {
    match os {
        "macos" => "~/Library/Application Support/Claude/claude_desktop_config.json",
        "windows" => "%APPDATA%\\Claude\\claude_desktop_config.json",
        _ => "~/.config/Claude/claude_desktop_config.json",
    }
}

/// POSIX-shell single-quoting, when needed.
fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `opts.env` (minus any [`TOKEN_ENV`]) followed by the token.
fn stdio_env<'a>(token: &'a str, opts: &'a SnippetOptions) -> Vec<(&'a str, &'a str)> {
    opts.env
        .iter()
        .filter(|(k, _)| k != TOKEN_ENV)
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .chain([(TOKEN_ENV, token)])
        .collect()
}

/// The `mcpServers` entry shared by Claude Desktop and Cursor.
fn stdio_server_json(client: &McpClientInfo, token: &str, opts: &SnippetOptions) -> String {
    let env: serde_json::Map<String, serde_json::Value> = stdio_env(token, opts)
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.into()))
        .collect();
    let v = json!({
        "mcpServers": {
            SERVER_KEY: {
                "command": opts.nuggetd,
                "args": ["mcp", "--client", client.id],
                "env": env
            }
        }
    });
    serde_json::to_string_pretty(&v).expect("json")
}

/// Setup snippets for `claude-desktop`, `claude-code`, `cursor`, `generic-stdio` and, when the
/// HTTP listener is on, `generic-http`.
pub fn setup_snippets(
    client: &McpClientInfo,
    token: &str,
    opts: &SnippetOptions,
) -> Vec<McpSetupSnippet> {
    let mut out = vec![
        McpSetupSnippet {
            app: "claude-desktop".into(),
            config_path: Some(claude_desktop_config_path(&opts.os).into()),
            text: stdio_server_json(client, token, opts),
        },
        McpSetupSnippet {
            app: "claude-code".into(),
            config_path: None,
            text: format!(
                "claude mcp add --transport stdio --scope user{} {SERVER_KEY} -- {} mcp --client {}",
                stdio_env(token, opts)
                    .into_iter()
                    .map(|(k, v)| format!(" --env {k}={}", sh_quote(v)))
                    .collect::<String>(),
                sh_quote(&opts.nuggetd),
                sh_quote(&client.id)
            ),
        },
        McpSetupSnippet {
            app: "cursor".into(),
            // Global config; a project's `.cursor/mcp.json` takes the same JSON.
            config_path: Some(if opts.os == "windows" {
                "%USERPROFILE%\\.cursor\\mcp.json".into()
            } else {
                "~/.cursor/mcp.json".into()
            }),
            text: stdio_server_json(client, token, opts),
        },
        McpSetupSnippet {
            app: "generic-stdio".into(),
            config_path: None,
            text: format!(
                "command: {} mcp --client {}{}",
                opts.nuggetd,
                client.id,
                stdio_env(token, opts)
                    .into_iter()
                    .map(|(k, v)| format!("\nenv: {k}={v}"))
                    .collect::<String>()
            ),
        },
    ];
    if let Some(port) = opts.http_port {
        out.push(McpSetupSnippet {
            app: "generic-http".into(),
            config_path: None,
            text: format!(
                "url: http://127.0.0.1:{port}{MCP_PATH}\nheader: Authorization: Bearer {token}\n\
                 transport: streamable HTTP (loopback only)"
            ),
        });
    }
    out
}

/// The `mcp.clients.create` IPC result: the client, its token (shown once) and setup snippets.
pub fn client_created(
    client: McpClientInfo,
    token: &ClientToken,
    opts: &SnippetOptions,
) -> McpClientCreated {
    let setup = setup_snippets(&client, token.expose(), opts);
    McpClientCreated {
        client,
        token: token.expose().to_owned(),
        setup,
    }
}
