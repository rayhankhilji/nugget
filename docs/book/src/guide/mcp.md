# Connecting AI apps over MCP

Nugget includes a [Model Context Protocol](https://modelcontextprotocol.io) server, so apps such as
Claude Desktop, Claude Code and Cursor can search your knowledge and cite it.

The daemon serves MCP whenever `[mcp] enabled` is on, which is the default (see
[config.toml](../reference/config.md#mcp)). The quickest way to connect an app is one command:

```sh
nuggetd mcp install --client claude-desktop
```

`--client` is `claude-desktop`, `claude-code` or `cursor`. The daemon must be running.

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
  the app's config (`nuggetd mcp install`) or shows it with a ready setup snippet (the desktop app).
  The daemon keeps only a keyed hash of it. If you lose it, revoke the client and create a new one.
- **Local only.** Apps connect through `nuggetd mcp` over stdio, or to a listener on `127.0.0.1`. There
  is no remote access.
- **Revocable, audited, rate-limited.** Revoking a client on **Connected AI apps** cuts it off on its
  next request. Every call is in the audit log (arguments summarised, item references, never
  content), and each client is limited to bursts of 60 requests and 120 per minute.

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
differently. To set it up by hand instead, add the snippet from **Connect an app** to the config
file:

| OS | Config file |
|---|---|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |
| Linux | `~/.config/Claude/claude_desktop_config.json` |

```json
{
  "mcpServers": {
    "nugget": {
      "command": "nuggetd",
      "args": ["mcp", "--client", "<client id>"],
      "env": { "NUGGET_MCP_TOKEN": "<token>" }
    }
  }
}
```

If the file already has an `mcpServers` object, add the `"nugget"` entry to it. If `nuggetd` is not on
your `PATH`, use its full path as `command`; the snippet the app shows already does. If the daemon
runs with `--home` or `NUGGET_HOME`, the snippet also sets `NUGGET_HOME` in `env`.

## Claude Code

```sh
nuggetd mcp install --client claude-code
```

This creates the client and prints the command that adds Nugget to Claude Code; it does not run it.
The command contains the token, so run it yourself and do not paste it anywhere else. It looks like
this:

```sh
claude mcp add --transport stdio --scope user --env NUGGET_MCP_TOKEN=<token> nugget -- nuggetd mcp --client <client id>
```

`--scope user` makes Nugget available in every project. Check it with `claude mcp list`.

## Cursor

```sh
nuggetd mcp install --client cursor
```

This works like the Claude Desktop install, on `~/.cursor/mcp.json` (Windows:
`%USERPROFILE%\.cursor\mcp.json`). For one project only, pass `--config <project>/.cursor/mcp.json`,
or add the same JSON as for Claude Desktop to that file by hand.

## Other apps

- **stdio**: run `nuggetd mcp --client <client id>` with `NUGGET_MCP_TOKEN=<token>` in its
  environment.
- **Streamable HTTP**: `http://127.0.0.1:7319/mcp` with the header `Authorization: Bearer <token>`.
  The listener is on by default (`[mcp] http`, port `[mcp] port`), and binds `127.0.0.1` only. If
  the port is taken, the daemon logs an error, serves stdio only and offers no HTTP snippet (it
  never points a client at a port it does not hold). Browsers are refused (any
  request with an `Origin` header is rejected unless that origin is listed in
  `[mcp] allowed_origins`), and the token is never accepted in the URL. The desktop's
  **Connect an app** shows a ready HTTP snippet with the actual port.

## Managing access

**Connected AI apps** in the desktop app lists every client with its scopes and when it was last
used, and revokes them. The same is available over IPC as `mcp.clients.list`, `mcp.clients.create`
and `mcp.clients.revoke` (see [IPC protocol](../reference/ipc.md#mcp-clients)). To turn MCP off
entirely, set `enabled = false` under `[mcp]` and restart the daemon.

## Why the token is in an environment variable

`nuggetd mcp --client <id>` is a thin pipe: it connects to the running daemon over the local socket,
presents the client id and token once, then copies MCP messages. Scopes, revocation, rate limits and audit are all enforced
inside the daemon, not in the process the app launched. The token goes in the environment rather than
the command line so it does not show up in process listings, and never in a URL, where it would leak
into logs and history.

## Prompt injection

Anything Nugget returns from your sources (an email, a web page you saved) could contain text written
to manipulate an AI. Nugget wraps every piece of source content in a labelled
`<untrusted-content>` block that the content cannot close early, and each tool's description tells the
model to treat it as data. That reduces the risk; it cannot remove it. Give apps only the sources they
need.
