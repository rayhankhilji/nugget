# MCP tools and scopes

The server in `nugget-mcp`, on unless `[mcp] enabled = false` ([config](config.md#mcp)). Setup:
[Connecting AI apps over MCP](../guide/mcp.md). Clients are managed on the **Connected AI apps** page,
with `nuggetd mcp install` ([Command line](cli.md#nuggetd-mcp)) or the `mcp.clients.*` IPC methods.

## Transports

| Transport | How a client connects | Credential |
|---|---|---|
| stdio | The app launches `nuggetd mcp --client <id>`, which pipes MCP to the running daemon over the local IPC socket | `NUGGET_MCP_TOKEN` in the launched process's environment |
| Streamable HTTP | `http://127.0.0.1:7319/mcp` when `[mcp] http` is on (the default). The port is `[mcp] port`; `0` picks a free one | `Authorization: Bearer ngt_…` on every request |

There is no remote transport. HTTP requests must come from loopback, use path `/mcp` with no query
string, carry `Host` `127.0.0.1:<port>` or `localhost:<port>`, have no `Origin` unless it is in
`[mcp] allowed_origins` (CORS is closed), present a live bearer token (else 401) and stay within the
rate limit (else 429). The service is stateless; bodies are capped at 1 MiB.

## Tools

All tools are read-only and idempotent.

<!-- check-refs: mcp.tools -->
| Tool | Arguments | Returns |
|---|---|---|
| `search` | `query` (1 to 2000 characters), `limit` (1 to 50, default 10) | The best passage of each matching item, with its source, item key and score, best first |
| `get_context` | `query`, `budget_tokens` (100 to 8000, default 2000) | Relevant, diverse passages packed under the budget and numbered `[1]`, `[2]`, …, for the model to cite |
| `find_entities` | `text` (name or alias fragment, up to 200 characters; empty lists the most recently updated), `kind`, `limit` (1 to 100, default 20) | Matching entities with their ids |
| `get_entity` | `id` | Kind, aliases, identifiers, facts with numbered citations and quotes, and mentions |
| `get_neighbourhood` | `id`, `depth` (1 to 3, default 1) | Connected entities (at most 100) and the relations between them |
| `get_item` | `source`, `key`, `offset_chars` (default 0), `max_chars` (1 to 100000, default 20000) | One item's text and metadata, in windows for long items |
| `recent_changes` | `since` (RFC 3339, default 7 days ago), `limit` (1 to 200, default 50) | Entities and items that changed, newest first |

`kind` is one of `person`, `organization`, `project`, `commitment`, `preference`, `event`, `place`,
`topic`.

Resources: `nugget://entity/<id>` (the entity's page, Markdown) and `nugget://item/<source>/<key>`.

## Scopes

Each client has scopes (`McpScopes` in the IPC protocol):

<!-- check-refs: mcp.scopes -->
| Scope | Type | Default | Effect |
|---|---|---|---|
| `sources` | list of source ids, or `null` | `null` (all) | Content is returned only from these sources. The pseudo-source `vault` covers facts you wrote yourself and entities with no source evidence |
| `tools` | list of tool names | empty (all read-only tools) | Only these tools are listed and callable |
| `include_sensitive` | bool | `false` | Whether items marked sensitive may be returned |

Scopes apply to every result: a fact keeps only the evidence the client may see and is dropped when
none is left; an entity's aliases, identifiers and attributes are shown only when every origin is
allowed.

## Tokens, limits and audit

- A token is `ngt_` plus 32 random bytes (base64url), returned once by `mcp.clients.create`. Only
  `HMAC-SHA256(server key, token)` is stored. Revoking deletes the record; the token stops working at
  once on every transport.
- Rate limit per client: a burst of 60, refilled at 120 per minute.
- Every `tools/call` and `resources/read` is audited: client, tool, outcome and item references, never
  content. Free-text arguments are kept only as length and a keyed hash. MCP is local, so it creates no
  egress rows.
- Source content is returned inside `<untrusted-content …>` blocks that the content cannot close or
  forge, and tool descriptions tell the model to treat it as data.
