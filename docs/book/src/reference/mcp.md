# MCP tools and scopes

Reference for the server in `nugget-mcp`. For setup steps, see
[Connecting AI apps over MCP](../guide/mcp.md).

`nuggetd` runs the server unless `[mcp] enabled = false` (see
[config.toml](config.md#mcp)). Clients are managed with the `mcp.clients.*` IPC methods, the
desktop's **Connected AI apps** page, or `nuggetd mcp install` ([Command line](cli.md#nuggetd-mcp)).

## Transports

| Transport | How a client connects | Credential |
|---|---|---|
| stdio | The app launches `nuggetd mcp --client <id>`, which pipes MCP to the running daemon over the local IPC socket | `NUGGET_MCP_TOKEN` in the launched process's environment |
| Streamable HTTP | `http://127.0.0.1:7319/mcp` when `[mcp] http` is on (the default). The port is `[mcp] port`; `0` picks a free one | `Authorization: Bearer ngt_…` on every request |

There is no remote transport. Every HTTP request passes these checks, in order:

1. The peer is on loopback (the listener binds `127.0.0.1` only).
2. The path is `/mcp`, with **no query string at all**: credentials never travel in URLs.
3. `Host` is `127.0.0.1:<port>` or `localhost:<port>` (DNS-rebinding protection).
4. `Origin`, if present, is on the allow-list (`[mcp] allowed_origins`), which is empty by default,
   so browser pages are refused.
   CORS is closed: preflights get 403, and no response carries `Access-Control-*` headers.
5. The bearer token names a live client (otherwise 401).
6. The client is within its rate limit (otherwise 429 with `Retry-After`).

The HTTP service is stateless: no MCP sessions, and each request carries its own credential. Request
bodies are limited to 1 MiB.

## Tools

All tools are read-only, idempotent and closed-world, and are annotated as such.

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

## Resources

| URI | Content |
|---|---|
| `nugget://entity/<id>` | The entity's page, as Markdown |
| `nugget://item/<source>/<key>` | One item |

## Scopes

Each client has scopes (`McpScopes` in the IPC protocol), applied to **every result**, whatever the
backend returned:

<!-- check-refs: mcp.scopes -->
| Scope | Type | Default | Effect |
|---|---|---|---|
| `sources` | list of source ids, or `null` | `null` (all) | Content is returned only from these sources. The pseudo-source `vault` covers facts you wrote yourself and entities with no source evidence |
| `tools` | list of tool names | empty (all read-only tools) | Only these tools are listed and callable |
| `include_sensitive` | bool | `false` | Whether items marked sensitive may be returned |

How scopes apply to the graph:

- A fact keeps only the evidence the client may see. A fact left with no visible evidence is dropped, so
  a fact backed only by a sensitive item never reaches the client.
- An entity is visible when at least one of its origins is allowed. Its aliases, identifiers and
  attributes have no per-field provenance, so they are shown only when **every** origin is allowed;
  otherwise they are withheld and the result says so.

## Tokens

- A token is `ngt_` followed by 32 random bytes (base64url). It is returned once, by
  `mcp.clients.create`.
- The daemon stores only `HMAC-SHA256(server key, token)`, so a leaked client table cannot be replayed
  or brute-forced offline without the server key. Lookups go by that hash, so comparisons leak nothing
  useful through timing.
- Revoking deletes the record: the token stops working on the next request, on every transport.

## Rate limits

A token bucket per client: a burst of 60 requests, refilled at 120 per minute.

## Audit

Every `tools/call` and `resources/read` produces one audit entry: the client, the tool, a summary of
the arguments (long strings clipped), the outcome and references to the items returned, never their
content. MCP is local only, so these calls do not create egress-ledger rows.

## Untrusted content

Everything that comes from your sources (passages, titles, file names, entity names, extracted facts)
is returned inside `<untrusted-content …>` blocks whose attributes say where it came from. Inside a
block, anything that looks like the block's own opening or closing tag is escaped, so content cannot
end its block early or forge a new one. Every tool description tells the model to treat block content
as data, not instructions.
