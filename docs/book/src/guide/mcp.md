# Connecting AI apps over MCP

Nugget includes a [Model Context Protocol](https://modelcontextprotocol.io) server, so apps such as
Claude Desktop, Claude Code, Cursor, VS Code and Zed can search your knowledge and cite it.

The daemon serves MCP whenever `[mcp] enabled` is on, which is the default (see
[config.toml](../reference/config.md#mcp)). The quickest way to connect an app is one command:

```sh
nuggetd mcp install --client claude-desktop
```

`--client` is `claude-desktop`, `claude-code` or `cursor`. The daemon must be running. For any other
app, create a client in the desktop app (**Connected AI apps**, **Connect an app**) and paste one of
the snippets below.

## What an app can do

Only read. There are seven read-only tools (`search`, `get_context`, `find_entities`, `get_entity`,
`get_neighbourhood`, `get_item`, `recent_changes`) and two kinds of resources (entity pages and items).
No tool can change your data, send anything or act on your computer. See
[MCP tools and scopes](../reference/mcp.md).

## How access works

- **One client per app.** Each app gets its own MCP client in Nugget, with its own token and scopes.
  `nuggetd mcp install` creates one with the default scopes (all sources, all read-only tools, no
  sensitive items). To choose the scopes yourself, open **Connected AI apps** in the desktop app,
  choose **Connect an app**, name it ("Claude Desktop"), and choose:
  - **Sources**: all, or only some (for example your notes but not your mail);
  - **Tools**: all read-only tools, or a subset;
  - **Sensitive items**: excluded unless you allow them.
- **A token, shown once.** Nugget creates a token (`ngt_…`) for the client and either writes it into
  the app's config (`nuggetd mcp install`) or shows it with ready setup snippets (the desktop app).
  The daemon keeps only a keyed hash of it. If you lose it, revoke the client and create a new one.
- **Local only.** Apps connect through `nuggetd mcp` over stdio, or to a listener on `127.0.0.1`. There
  is no remote access.
- **Revocable, audited, rate-limited.** Revoking a client on **Connected AI apps** cuts it off on its
  next request. Every tool call and resource read is in the audit log (arguments summarised, item
  references, never content; cancelled calls too), and each client is limited to bursts of 60
  requests and 120 per minute.

## Stdio or HTTP?

Use **stdio** (the `nuggetd mcp --client <id>` bridge) whenever the app supports it; every snippet
below that launches a command does. The app starts `nuggetd mcp`, which reaches the running daemon
over its user-private local socket and presents the token there. The token never goes over TCP, so
no other program can collect it.

**HTTP** (`http://127.0.0.1:7319/mcp` with an `Authorization: Bearer` header) is the fallback for apps
that cannot launch a command. Plain loopback HTTP cannot prove the server is Nugget: if the daemon is
stopped and another local program takes the port, an app configured for HTTP sends it the token.
Nugget never binds a different port and never offers an HTTP snippet for a port it does not hold, but
it cannot protect a config that already points at the port. Prefer stdio.

In the snippets below, replace `mcp_YOUR_CLIENT_ID` and `ngt_YOUR_TOKEN` with your client's id and
token (the desktop app's snippets have them filled in). If `nuggetd` is not on the `PATH` the app
sees, use its full path as the command (on Windows the full path of `nuggetd.exe`; in JSON, double
each backslash). If the daemon runs with `--home` or `NUGGET_HOME`, add `NUGGET_HOME` with the same
directory to `env`; the desktop app's snippets already include it.

## Claude Desktop

```sh
nuggetd mcp install --client claude-desktop
```

This shows what it will do and asks you to confirm (`--yes` skips the question), creates the client,
and adds a `nugget` entry under `mcpServers` in Claude Desktop's config file. Everything else in the
file is kept, the previous version is saved next to it as `<file>.nugget-backup-<time>`, and the new
file is readable only by you, since it holds the token. Running it again replaces the entry and
revokes the client it named. Restart Claude Desktop afterwards.

Use `--config PATH` if your config file is somewhere else, and `--name NAME` to name the client
differently. To set it up by hand instead, add this to the config file (if it already has an
`mcpServers` object, add the `"nugget"` entry to it) and restart Claude Desktop:

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

This creates the client and prints the command that adds Nugget to Claude Code; it does not run it.
The command contains the token, so run it yourself and do not paste it anywhere else. It looks like
this:

```sh
claude mcp add --transport stdio --scope user --env NUGGET_MCP_TOKEN=ngt_YOUR_TOKEN nugget -- nuggetd mcp --client mcp_YOUR_CLIENT_ID
```

`--scope user` makes Nugget available in every project. Check it with `claude mcp list`. Over HTTP
instead (see [Stdio or HTTP?](#stdio-or-http)):

```sh
claude mcp add --transport http --scope user nugget http://127.0.0.1:7319/mcp --header "Authorization: Bearer ngt_YOUR_TOKEN"
```

## Cursor

```sh
nuggetd mcp install --client cursor
```

This works like the Claude Desktop install, on `~/.cursor/mcp.json` (Windows:
`%USERPROFILE%\.cursor\mcp.json`). For one project only, pass `--config <project>/.cursor/mcp.json`,
or add the same JSON as for Claude Desktop to that file by hand. Cursor can also connect over HTTP:

```json
{
  "mcpServers": {
    "nugget": {
      "url": "http://127.0.0.1:7319/mcp",
      "headers": {
        "Authorization": "Bearer ngt_YOUR_TOKEN"
      }
    }
  }
}
```

## VS Code

VS Code (agent mode in GitHub Copilot Chat) reads MCP servers from `.vscode/mcp.json` in a
workspace, or from your user configuration (**MCP: Open User Configuration** in the command palette).
Its top-level key is `servers`, not `mcpServers`. This version asks for the token once and keeps it
in VS Code's secret storage instead of the file:

```json
{
  "inputs": [
    {
      "type": "promptString",
      "id": "nugget-token",
      "description": "Nugget MCP token (ngt_...)",
      "password": true
    }
  ],
  "servers": {
    "nugget": {
      "type": "stdio",
      "command": "nuggetd",
      "args": ["mcp", "--client", "mcp_YOUR_CLIENT_ID"],
      "env": {
        "NUGGET_MCP_TOKEN": "${input:nugget-token}"
      }
    }
  }
}
```

Over HTTP, the server entry is:

```json
{
  "servers": {
    "nugget": {
      "type": "http",
      "url": "http://127.0.0.1:7319/mcp",
      "headers": {
        "Authorization": "Bearer ngt_YOUR_TOKEN"
      }
    }
  }
}
```

## Zed

Add a custom context server to Zed's `settings.json` (**zed: open settings**). Zed's settings format
has changed between releases; if this one is not accepted, add a custom server from the Agent
panel's settings with the same command, arguments and environment.

```json
{
  "context_servers": {
    "nugget": {
      "source": "custom",
      "command": "nuggetd",
      "args": ["mcp", "--client", "mcp_YOUR_CLIENT_ID"],
      "env": {
        "NUGGET_MCP_TOKEN": "ngt_YOUR_TOKEN"
      }
    }
  }
}
```

## Other apps

Any MCP app that launches a local command (stdio):

```text
command: nuggetd mcp --client mcp_YOUR_CLIENT_ID
env: NUGGET_MCP_TOKEN=ngt_YOUR_TOKEN
```

Any MCP app that speaks streamable HTTP:

```text
url: http://127.0.0.1:7319/mcp
header: Authorization: Bearer ngt_YOUR_TOKEN
transport: streamable HTTP (loopback only)
```

The listener is on by default (`[mcp] http`, port `[mcp] port`), and binds `127.0.0.1` only. If the
port is taken, the daemon logs an error, serves stdio only and offers no HTTP snippet (it never points
a client at a port it does not hold). Browsers are refused (any request with an `Origin` header is
rejected unless that origin is listed in `[mcp] allowed_origins`), and the token is never accepted in
the URL. Every request needs the header; the server keeps no sessions. The desktop's **Connect an
app** shows a ready HTTP snippet with the actual port.

## Tested clients

CI (`.github/workflows/mcp-compat.yml`) runs the official MCP SDK clients against a real `nuggetd`
on every change to the MCP server: the TypeScript SDK (`@modelcontextprotocol/sdk` 1.32.2) and the
Python SDK (`mcp` 2.3.0). Each connects over the stdio bridge, launched with exactly the `command`,
`args` and `env` of the Claude Desktop snippet above, and over HTTP with the URL and header of the
generic HTTP snippet. They list and call every tool (valid and invalid arguments), page through
`resources/list`, read entity and item resources, cancel requests in flight, and check the errors for
unknown tools and resources, revoked clients, wrong tokens and rate limits. Protocol revisions
tested: 2025-06-18 and 2025-11-25 (TypeScript), 2025-11-25 and 2026-07-28 (Python). The clients are
in `crates/nugget-mcp/compat`; a test in `crates/nugget-mcp` checks that the snippets on this page
are the ones the daemon generates.

## Errors an app may show

| What the app reports | Meaning |
|---|---|
| `nuggetd is not running` (stdio; the app's MCP log) | Start Nugget, then reconnect the app. |
| `unknown or revoked MCP client, or wrong token` (stdio) / HTTP 401 `missing or invalid bearer token` | The client was revoked or the token is wrong. Create a new client. |
| `rate limited; retry after N s` (HTTP 429 with `Retry-After`) | More than 60 requests at once, or over 120 a minute. |
| A tool result marked as an error (`invalid arguments`, `entity not found`) | The call reached Nugget; the model can fix its arguments and retry. |
| JSON-RPC `-32602` | Unknown tool, or a `resources/list` cursor Nugget did not hand out (start the listing again). |
| JSON-RPC `-32002` (`-32602` from protocol 2026-07-28) | Unknown resource URI, or one outside the client's scopes. |
| HTTP 403 `forbidden origin` | A browser page tried to connect. Add its origin to `[mcp] allowed_origins` only if you trust it (for example the MCP Inspector). |

## Managing access

**Connected AI apps** in the desktop app lists every client with its scopes and when it was last
used, and revokes them. The same is available over IPC as `mcp.clients.list`, `mcp.clients.create`
and `mcp.clients.revoke` (see [IPC protocol](../reference/ipc.md#mcp-clients)). To turn MCP off
entirely, set `enabled = false` under `[mcp]` and restart the daemon.

## Why the token is in an environment variable

`nuggetd mcp --client <id>` is a thin pipe: it connects to the running daemon over the local socket,
presents the client id and token once, then copies MCP messages. Scopes, revocation, rate limits and
audit are all enforced inside the daemon, not in the process the app launched. The token goes in the
environment rather than the command line so it does not show up in process listings, and never in a
URL, where it would leak into logs and history.

## Prompt injection

Anything Nugget returns from your sources (an email, a web page you saved) could contain text written
to manipulate an AI. Nugget wraps every piece of source content in a labelled
`<untrusted-content>` block that the content cannot close early, and each tool's description tells the
model to treat it as data. That reduces the risk; it cannot remove it. Give apps only the sources they
need.
