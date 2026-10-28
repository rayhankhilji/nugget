# Connecting AI apps over MCP

Nugget includes a [Model Context Protocol](https://modelcontextprotocol.io) server, so apps such as
Claude Desktop, Claude Code and Cursor can search your knowledge and cite it.

> **Status: built, not yet running in the daemon.** The MCP server (`nugget-mcp`) and the desktop's
> **Connected apps** page are implemented and tested, but `nuggetd` does not start the server yet:
> there is no `nuggetd mcp` subcommand and no `mcp.clients.*` IPC method in the daemon today. The
> snippets below are exactly what the server generates; they will work once the daemon integration
> (milestone M2, wave 2) lands.

## What an app can do

Only read. There are seven read-only tools (`search`, `get_context`, `find_entities`, `get_entity`,
`get_neighbourhood`, `get_item`, `recent_changes`) and two kinds of resources (entity pages and items).
No tool can change your data, send anything or act on your computer. See
[MCP tools and scopes](../reference/mcp.md).

## How access works

- **One client per app.** On **Connected apps**, choose **Connect an app**, name it ("Claude Desktop"),
  and choose its scopes:
  - **Sources**: all, or only some (for example your notes but not your mail);
  - **Tools**: all read-only tools, or a subset;
  - **Sensitive items**: excluded unless you allow them.
- **A token, shown once.** Nugget creates a token (`ngt_…`) for the client and shows it with a ready
  setup snippet. The daemon keeps only a keyed hash of it. If you lose it, revoke the client and create
  a new one.
- **Local only.** Apps connect through `nuggetd mcp` over stdio, or to a listener on `127.0.0.1`. There
  is no remote access.
- **Revocable, audited, rate-limited.** Revoking a client cuts it off immediately. Every call is in the
  audit log (arguments summarised, item references, never content), and each client is limited to 60
  requests at once and 120 per minute.

## Claude Desktop

Add this to Claude Desktop's config file, then restart Claude Desktop:

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
your `PATH`, use its full path as `command`; the snippet the app shows already does.

## Claude Code

Run this once in a terminal:

```sh
claude mcp add --transport stdio --scope user --env NUGGET_MCP_TOKEN=<token> nugget -- nuggetd mcp --client <client id>
```

`--scope user` makes Nugget available in every project. Check it with `claude mcp list`.

## Cursor

Add the same JSON as for Claude Desktop to `~/.cursor/mcp.json` (Windows:
`%USERPROFILE%\.cursor\mcp.json`), or to a project's `.cursor/mcp.json`.

## Other apps

- **stdio**: run `nuggetd mcp --client <client id>` with `NUGGET_MCP_TOKEN=<token>` in its
  environment.
- **Streamable HTTP**: `http://127.0.0.1:7466/mcp` with the header `Authorization: Bearer <token>`.
  Browsers are refused (any request with an `Origin` header is rejected unless that origin is
  explicitly allowed), and the token is never accepted in the URL.

## Why the token is in an environment variable

`nuggetd mcp` is a thin pipe: it connects to the running daemon over the local socket, presents the
client id and token once, then copies bytes. Scopes, revocation, rate limits and audit are all enforced
inside the daemon, not in the process the app launched. The token goes in the environment rather than
the command line so it does not show up in process listings, and never in a URL, where it would leak
into logs and history.

## Prompt injection

Anything Nugget returns from your sources (an email, a web page you saved) could contain text written
to manipulate an AI. Nugget wraps every piece of source content in a labelled
`<untrusted-content>` block that the content cannot close early, and each tool's description tells the
model to treat it as data. That reduces the risk; it cannot remove it. Give apps only the sources they
need.
