# Connecting AI apps over MCP

Heed includes a [Model Context Protocol](https://modelcontextprotocol.io) server, so apps such as
Claude Desktop, Claude Code, Cursor, VS Code and Zed can search your knowledge and cite it. MCP is on
by default (`[mcp] enabled`, see [config.toml](../reference/config.md#mcp)). With the daemon running,
the quickest way to connect an app is:

```sh
nuggetd mcp install --client claude-desktop
```

`--client` is `claude-desktop`, `claude-code` or `cursor`. For any other app, create a client in the
desktop app (**Connected AI apps**, **Connect an app**) and paste one of the snippets below.

## What an app can do

Only read: seven read-only tools and two resource kinds (see
[MCP tools and scopes](../reference/mcp.md)). Each app gets its own client with its own token
(`ngt_…`, shown once) and scopes. `nuggetd mcp install` uses the defaults (all sources, all tools, no
sensitive items); **Connect an app** in the desktop app lets you choose. Access is local only,
audited, rate-limited and revocable on **Connected AI apps**. If you lose a token, revoke the client
and create a new one.

**Prefer stdio** (`nuggetd mcp --client <id>`): the token goes to the daemon over a private local
socket. Use HTTP (`http://127.0.0.1:7319/mcp`) only for apps that can't launch a command: if the
daemon is stopped and another program takes the port, the app would send it the token.

In the snippets, replace `mcp_YOUR_CLIENT_ID` and `ngt_YOUR_TOKEN` (the desktop app fills them in).
If `nuggetd` is not on the app's `PATH`, use its full path. If the daemon runs with `--home` or
`NUGGET_HOME`, add `NUGGET_HOME` to `env` too.

## Claude Desktop

```sh
nuggetd mcp install --client claude-desktop
```

This adds a `nugget` entry to Claude Desktop's config (keeping a `<file>.nugget-backup-<time>`
copy); `--yes` skips the confirmation, `--config PATH` and `--name NAME` override the file and client
name. Restart Claude Desktop afterwards. By hand, add this to the config file:

| OS | Config file |
|---|---|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |
| Linux | `~/.config/Claude/claude_desktop_config.json` |

```json
{
  "mcpServers": {
    "nugget": {
      "args": [
        "mcp",
        "--client",
        "mcp_YOUR_CLIENT_ID"
      ],
      "command": "nuggetd",
      "env": {
        "NUGGET_MCP_TOKEN": "ngt_YOUR_TOKEN"
      }
    }
  }
}
```

## Claude Code

```sh
nuggetd mcp install --client claude-code
```

This creates the client and prints the command to run yourself (it contains the token and lands in
your shell history):

```sh
claude mcp add --transport stdio --scope user --env NUGGET_MCP_TOKEN=ngt_YOUR_TOKEN nugget -- nuggetd mcp --client mcp_YOUR_CLIENT_ID
```

`--scope user` makes Heed available in every project; check with `claude mcp list`. Over HTTP:

```sh
claude mcp add --transport http --scope user nugget http://127.0.0.1:7319/mcp --header "Authorization: Bearer ngt_YOUR_TOKEN"
```

## Cursor

```sh
nuggetd mcp install --client cursor
```

Works like the Claude Desktop install, on `~/.cursor/mcp.json` (Windows:
`%USERPROFILE%\.cursor\mcp.json`); pass `--config <project>/.cursor/mcp.json` for one project. Over
HTTP:

```json
{
  "mcpServers": {
    "nugget": {
      "url": "http://127.0.0.1:7319/mcp",
      "headers": { "Authorization": "Bearer ngt_YOUR_TOKEN" }
    }
  }
}
```

## VS Code

VS Code (Copilot Chat agent mode) reads `.vscode/mcp.json` or your user configuration (**MCP: Open
User Configuration**). The top-level key is `servers`. These versions keep the token in VS Code's
secret storage:

```json
{
  "inputs": [
    { "type": "promptString", "id": "nugget-token", "description": "Heed MCP token (ngt_...)", "password": true }
  ],
  "servers": {
    "nugget": {
      "type": "stdio",
      "command": "nuggetd",
      "args": ["mcp", "--client", "mcp_YOUR_CLIENT_ID"],
      "env": { "NUGGET_MCP_TOKEN": "${input:nugget-token}" }
    }
  }
}
```

Over HTTP:

```json
{
  "inputs": [
    { "type": "promptString", "id": "nugget-token", "description": "Heed MCP token (ngt_...)", "password": true }
  ],
  "servers": {
    "nugget": {
      "type": "http",
      "url": "http://127.0.0.1:7319/mcp",
      "headers": { "Authorization": "Bearer ${input:nugget-token}" }
    }
  }
}
```

## Zed

Add to Zed's `settings.json` (**zed: open settings**). If your Zed version rejects it, add a custom
server from the Agent panel's settings with the same command, arguments and environment.

```json
{
  "context_servers": {
    "nugget": {
      "source": "custom",
      "command": "nuggetd",
      "args": ["mcp", "--client", "mcp_YOUR_CLIENT_ID"],
      "env": { "NUGGET_MCP_TOKEN": "ngt_YOUR_TOKEN" }
    }
  }
}
```

## Other apps

Stdio:

```text
command: nuggetd mcp --client mcp_YOUR_CLIENT_ID
env: NUGGET_MCP_TOKEN=ngt_YOUR_TOKEN
```

Streamable HTTP:

```text
url: http://127.0.0.1:7319/mcp
header: Authorization: Bearer ngt_YOUR_TOKEN
transport: streamable HTTP (loopback only)
```

The HTTP listener (`[mcp] http`, `[mcp] port`) binds `127.0.0.1` only; browser origins are refused
unless listed in `[mcp] allowed_origins`.

## Errors an app may show

| What the app reports | Meaning |
|---|---|
| `nuggetd is not running` (stdio; the app's MCP log) | Start Heed, then reconnect the app. |
| `unknown or revoked MCP client, or wrong token` (stdio) / HTTP 401 `missing or invalid bearer token` | The client was revoked or the token is wrong. Create a new client. |
| `rate limited; retry after N s` (HTTP 429 with `Retry-After`) | More than 60 requests at once, or over 120 a minute. |
| A tool result marked as an error (`invalid arguments`, `entity not found`) | The call reached Heed; the model can fix its arguments and retry. |
| JSON-RPC `-32602` | Unknown tool, or a stale `resources/list` cursor (start the listing again). |
| JSON-RPC `-32002` (`-32602` from protocol 2026-07-28) | Unknown resource URI, or one outside the client's scopes. |
| HTTP 403 `forbidden origin` | A browser page tried to connect. Add its origin to `[mcp] allowed_origins` only if you trust it. |

## Managing access

**Connected AI apps** lists clients with their scopes and last use, and revokes them (IPC:
`mcp.clients.list`, `mcp.clients.create`, `mcp.clients.revoke`; see
[IPC protocol](../reference/ipc.md#mcp-clients)). To turn MCP off, set `enabled = false` under
`[mcp]` and restart the daemon.

**Prompt injection:** content from your sources could contain text written to manipulate an AI.
Heed marks it as untrusted data, which reduces the risk but cannot remove it, so give apps only the
sources they need.
