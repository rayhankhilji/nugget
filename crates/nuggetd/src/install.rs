//! `nuggetd mcp install --client claude-desktop|claude-code|cursor [--name N] [--yes]`.
//!
//! Creates an MCP client in the running daemon (`mcp.clients.create`) and sets the app up with
//! its snippet:
//! - **Claude Desktop, Cursor:** the app's JSON config gets an `mcpServers.nugget` entry. The file
//!   is merged, never clobbered: other servers and settings stay as they are, an unparsable file
//!   is left alone (error), the previous version is copied to `<file>.nugget-backup-<time>`, and
//!   the new one is written to a temporary file (0600: it holds the token) and renamed into
//!   place. A replaced `nugget` entry's old client is revoked.
//! - **Claude Code:** prints the `claude mcp add ...` command to run (it is not run for you).
//!
//! On a terminal the plan is shown and confirmed first; `--yes` skips that, and without a
//! terminal `--yes` is required.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use nugget_core::ipc::{McpClientCreated, McpScopes, method};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_ipc::Client;
use serde_json::{Map, Value, json};

/// Name the server is registered under in each app.
pub const SERVER_KEY: &str = nugget_mcp::snippets::SERVER_KEY;

/// An app `nuggetd mcp install` can set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum App {
    ClaudeDesktop,
    ClaudeCode,
    Cursor,
}

impl App {
    /// The snippet `app` name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "claude-desktop",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "Claude Desktop",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
        }
    }
}

/// Options of one install.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub app: App,
    /// Client name shown in Nugget (default: the app's name).
    pub name: Option<String>,
    /// Config file to merge into instead of the app's default location.
    pub config: Option<PathBuf>,
    /// Skip the confirmation.
    pub yes: bool,
}

/// Expand `~`, `%APPDATA%` and `%USERPROFILE%` at the start of a config path hint.
pub fn expand_path(hint: &str) -> Option<PathBuf> {
    let env_dir = |var: &str| std::env::var_os(var).map(PathBuf::from);
    let (base, rest) = if let Some(r) = hint.strip_prefix("~") {
        (std::env::home_dir()?, r)
    } else if let Some(r) = hint.strip_prefix("%APPDATA%") {
        (env_dir("APPDATA")?, r)
    } else if let Some(r) = hint.strip_prefix("%USERPROFILE%") {
        (env_dir("USERPROFILE").or_else(std::env::home_dir)?, r)
    } else {
        return Some(PathBuf::from(hint));
    };
    let rest = rest.trim_start_matches(['/', '\\']);
    Some(if rest.is_empty() {
        base
    } else {
        base.join(rest.replace('\\', std::path::MAIN_SEPARATOR_STR))
    })
}

/// The `mcpServers.nugget` entry of a Claude Desktop / Cursor snippet.
pub fn server_entry(snippet: &str) -> Result<Value> {
    let v: Value = serde_json::from_str(snippet)?;
    v.pointer(&format!("/mcpServers/{SERVER_KEY}"))
        .cloned()
        .ok_or_else(|| Error::Invalid("setup snippet has no mcpServers entry".into()))
}

/// What [`merge_config`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeOutcome {
    /// Copy of the previous file, if there was one.
    pub backup: Option<PathBuf>,
    /// The `nugget` entry that was replaced, if any.
    pub replaced: Option<Value>,
}

/// Set `mcpServers.<key> = entry` in the JSON config at `path`, keeping everything else. See the
/// module docs for the safety rules.
pub fn merge_config(path: &Path, key: &str, entry: Value) -> Result<MergeOutcome> {
    let existing = match std::fs::read(path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(Error::Invalid(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    let mut root = match &existing {
        Some(b) if !b.iter().all(u8::is_ascii_whitespace) => serde_json::from_slice::<Value>(b)
            .map_err(|e| {
                Error::Invalid(format!(
                    "{} is not valid JSON ({e}); it was not changed",
                    path.display()
                ))
            })?,
        _ => Value::Object(Map::new()),
    };
    let obj = root.as_object_mut().ok_or_else(|| {
        Error::Invalid(format!(
            "{} does not hold a JSON object; it was not changed",
            path.display()
        ))
    })?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers.as_object_mut().ok_or_else(|| {
        Error::Invalid(format!(
            "`mcpServers` in {} is not an object; it was not changed",
            path.display()
        ))
    })?;
    let replaced = servers.insert(key.to_owned(), entry);

    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let backup = match &existing {
        Some(bytes) => {
            let stamp = time::OffsetDateTime::now_utc().unix_timestamp();
            let mut n = 0u32;
            let b = loop {
                let mut name = path.file_name().unwrap_or_default().to_os_string();
                name.push(format!(".nugget-backup-{stamp}"));
                if n > 0 {
                    name.push(format!("-{n}"));
                }
                let b = path.with_file_name(name);
                if !b.exists() || n >= 100 {
                    break b;
                }
                n += 1;
            };
            write_private(&b, bytes, true)?;
            Some(b)
        }
        None => None,
    };
    let mut text = serde_json::to_string_pretty(&root)?;
    text.push('\n');
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".nugget-tmp-{}", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    write_private(&tmp, text.as_bytes(), true)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(MergeOutcome { backup, replaced })
}

/// Write a new file readable by the user only (0600 on Unix), synced to disk.
fn write_private(path: &Path, bytes: &[u8], create_new: bool) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true);
    if create_new {
        opts.create_new(true);
    } else {
        opts.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| Error::Invalid(format!("cannot write {}: {e}", path.display())))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// The client id in a `nugget` server entry (`args: ["mcp", "--client", <id>]`).
pub fn entry_client_id(entry: &Value) -> Option<String> {
    let args = entry.get("args")?.as_array()?;
    let at = args.iter().position(|a| a == "--client")?;
    args.get(at + 1)?.as_str().map(str::to_owned)
}

fn confirm(prompt: &str) -> Result<bool> {
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Run the install against the daemon at `paths`. Returns the text to print on stdout.
pub async fn install(paths: &Paths, opts: InstallOptions) -> Result<String> {
    let app = opts.app;
    let target = match app {
        App::ClaudeCode => None,
        App::ClaudeDesktop | App::Cursor => Some(match &opts.config {
            Some(p) => p.clone(),
            None => default_config_path(app).ok_or_else(|| {
                Error::Invalid(format!(
                    "cannot find {}'s config file; pass --config PATH",
                    app.display()
                ))
            })?,
        }),
    };
    let plan = match &target {
        Some(p) => format!(
            "Create a Nugget MCP client for {} and add the `{SERVER_KEY}` server to {} (other servers are kept; a backup is made).",
            app.display(),
            p.display()
        ),
        None => format!(
            "Create a Nugget MCP client for {} and print the `claude mcp add` command to run.",
            app.display()
        ),
    };
    if !opts.yes {
        if !std::io::stdin().is_terminal() {
            return Err(Error::Invalid(format!(
                "{plan}\nNot a terminal: pass --yes to confirm."
            )));
        }
        eprintln!("{plan}");
        if !confirm("Proceed?")? {
            return Ok("Cancelled; nothing was changed.".into());
        }
    }

    let client = Client::connect(paths).await.map_err(|e| {
        Error::Other(format!(
            "nuggetd is not running (no answer on {}): {e}",
            nugget_ipc::socket_display(paths)
        ))
    })?;
    let name = opts
        .name
        .clone()
        .unwrap_or_else(|| app.display().to_owned());
    let scopes = McpScopes {
        sources: None,
        tools: Vec::new(),
        include_sensitive: false,
    };
    let created: McpClientCreated = client
        .call(
            method::MCP_CLIENTS_CREATE,
            &json!({"name": name, "scopes": scopes}),
        )
        .await
        .map_err(|e| Error::Other(format!("cannot create the MCP client: {e}")))?;
    let snippet = created
        .setup
        .iter()
        .find(|s| s.app == app.as_str())
        .ok_or_else(|| Error::Other(format!("no setup snippet for {}", app.as_str())))?;

    let Some(path) = target else {
        return Ok(format!(
            "Created MCP client {} ({}). Run this command to add Nugget to Claude Code \
             (it contains the client's token; keep it private):\n\n{}\n",
            created.client.id, created.client.name, snippet.text
        ));
    };
    let entry = server_entry(&snippet.text)?;
    let outcome = match merge_config(&path, SERVER_KEY, entry) {
        Ok(o) => o,
        Err(e) => {
            // Do not leave a client nobody can use.
            let _ = client
                .call_value(method::MCP_CLIENTS_REVOKE, json!({"id": created.client.id}))
                .await;
            return Err(e);
        }
    };
    let mut out = format!(
        "Created MCP client {} ({}) and added the `{SERVER_KEY}` server to {}.",
        created.client.id,
        created.client.name,
        path.display()
    );
    if let Some(b) = &outcome.backup {
        out.push_str(&format!("\nBackup of the previous file: {}", b.display()));
    }
    if let Some(old) = outcome.replaced.as_ref().and_then(entry_client_id)
        && old != created.client.id
    {
        match client
            .call_value(method::MCP_CLIENTS_REVOKE, json!({"id": old}))
            .await
        {
            Ok(_) => out.push_str(&format!("\nRevoked the client it replaced ({old}).")),
            Err(_) => out.push_str(&format!(
                "\nReplaced an earlier entry (client {old}, no longer known)."
            )),
        }
    }
    out.push_str(&format!(
        "\nRestart {} to connect. Manage access in Nugget under Connected AI apps.\n",
        app.display()
    ));
    Ok(out)
}

/// The app's config file on this machine.
pub fn default_config_path(app: App) -> Option<PathBuf> {
    let hint = match (app, std::env::consts::OS) {
        (App::ClaudeDesktop, "macos") => {
            "~/Library/Application Support/Claude/claude_desktop_config.json"
        }
        (App::ClaudeDesktop, "windows") => "%APPDATA%\\Claude\\claude_desktop_config.json",
        (App::ClaudeDesktop, _) => "~/.config/Claude/claude_desktop_config.json",
        (App::Cursor, "windows") => "%USERPROFILE%\\.cursor\\mcp.json",
        (App::Cursor, _) => "~/.cursor/mcp.json",
        (App::ClaudeCode, _) => return None,
    };
    expand_path(hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_keep_other_servers_and_back_up() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("cfg").join("claude_desktop_config.json");
        // Fresh file.
        let first = json!({"command": "nuggetd", "args": ["mcp", "--client", "mcp_a"], "env": {"NUGGET_MCP_TOKEN": "t1"}});
        let o = merge_config(&p, "nugget", first.clone()).unwrap();
        assert_eq!(
            o,
            MergeOutcome {
                backup: None,
                replaced: None
            }
        );
        // Other content is kept; the entry is replaced.
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        v["mcpServers"]["other"] = json!({"command": "x"});
        v["theme"] = json!("dark");
        std::fs::write(&p, serde_json::to_string(&v).unwrap()).unwrap();
        let second = json!({"command": "nuggetd", "args": ["mcp", "--client", "mcp_b"]});
        let o = merge_config(&p, "nugget", second.clone()).unwrap();
        assert_eq!(
            o.replaced.as_ref().and_then(entry_client_id).as_deref(),
            Some("mcp_a")
        );
        let backup = o.backup.unwrap();
        assert!(
            std::fs::read_to_string(&backup)
                .unwrap()
                .contains("\"other\"")
        );
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["nugget"], second);
        assert_eq!(v["mcpServers"]["other"], json!({"command": "x"}));
        assert_eq!(v["theme"], "dark");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in [&p, &backup] {
                let mode = std::fs::metadata(f).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600, "{}", f.display());
            }
        }
        // Leftover temp files are gone.
        let names: Vec<_> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.contains("nugget-tmp")), "{names:?}");
    }

    #[test]
    fn refuses_to_clobber_bad_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("c.json");
        for bad in ["{not json", "[1, 2]", r#"{"mcpServers": 3}"#] {
            std::fs::write(&p, bad).unwrap();
            assert!(merge_config(&p, "nugget", json!({})).is_err(), "{bad}");
            assert_eq!(std::fs::read_to_string(&p).unwrap(), bad);
        }
        std::fs::write(&p, "  \n").unwrap();
        merge_config(&p, "nugget", json!({"command": "n"})).unwrap();
    }

    #[test]
    fn paths_and_entries() {
        assert_eq!(
            expand_path("/abs/x.json"),
            Some(PathBuf::from("/abs/x.json"))
        );
        if let Some(h) = std::env::home_dir() {
            assert_eq!(
                expand_path("~/.cursor/mcp.json"),
                Some(h.join(".cursor/mcp.json"))
            );
        }
        let snippet =
            r#"{"mcpServers": {"nugget": {"command": "n", "args": ["mcp", "--client", "mcp_x"]}}}"#;
        let e = server_entry(snippet).unwrap();
        assert_eq!(entry_client_id(&e).as_deref(), Some("mcp_x"));
        assert!(server_entry("{}").is_err());
    }
}
