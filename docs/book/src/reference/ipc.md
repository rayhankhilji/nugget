# IPC protocol

The desktop app, the CLI and any other local program talk to `nuggetd` with **JSON-RPC 2.0**, one JSON
object per line, over a local socket. The protocol types live in `nugget_core::ipc`; the Rust client is
`nugget_ipc::Client`.

| | |
|---|---|
| Protocol version | `1` (reported by `status`) |
| Framing | One JSON object per line (`\n`-terminated), UTF-8 |
| Maximum message | 16 MiB per line; larger messages get error `-32008` |
| macOS, Linux | Unix domain socket `<data>/ipc/nuggetd.sock`, in a directory with mode `0700`. If that path is too long for a socket, a short private directory `nugget-<hash>` under `$XDG_RUNTIME_DIR` (or the temp directory, ownership- and mode-checked) is used instead |
| Windows | Named pipe `\\.\pipe\nugget-<user SID>-<hash>-nuggetd.sock`, with an ACL that admits only you and SYSTEM. Clients verify that the server runs as the same user before sending anything |
| Network | None. There is no TCP listener |
| Authentication | Being able to open the socket (your user account) is the credential |

`nuggetd status` prints the socket path in use.

## Conventions

- Requests carry an `id`; calls on one connection may run concurrently and are matched by `id`.
- **Notifications** (no `id`) carry streaming output and events. `chat.delta`, `chat.done` and
  `models.progress` go to the connection that started the stream. `ingest.progress`,
  `sources.changed` and `knowledge.changed` are broadcast to every connection.
- Methods that take no parameters accept `{}` (or omitted `params`).
- Timestamps are RFC 3339 strings. Ids (`SourceId`, `ProviderId`, `EntityId`, `ItemKey`) are strings.
  An `ItemRef` is `{"source": "<source id>", "key": "<item key>"}`.
- Tiers: `triage`, `light`, `synthesis`, `agentic`, `embedding`. Localities: `on_device`,
  `local_network`, `cloud`.
- The **Daemon** column says whether today's `nuggetd` implements the method:
  - **yes**: the daemon serves it.
  - **not yet**: part of the protocol, and the desktop app may be built against it, but the daemon
    answers `-32601` (method not found).
  - **in progress**: being wired into the daemon in the current wave of work (wave W) on another
    branch. The name and the one-line description come from the plan; parameters and results are
    not final, and some of these names are not in `nugget_core::ipc` yet. Until the work lands, the
    daemon answers `-32601`.

## Methods

### Status, providers and models

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `status` | `{}` | `StatusResponse`: `protocol`, `version`, `uptime_secs`, `items`, `chunks`, `providers` (list of `ProviderInfo`), `audit_chain_ok` | yes |
| `providers.list` | `{}` | list of `ProviderInfo`: `id`, `kind`, `model`, `locality`, `tiers`, `capabilities` (`null` until probed), `error` (why it is unavailable) | yes |
| `providers.probe` | `ProbeRequest`: `{provider}` | `Capabilities`: `streaming`, `vision`, `tools`, `json_schema`, `embeddings`, `context_tokens` | yes |
| `models.list` | `{}` | list of `ModelInfo`: `id`, `size_bytes`, `installed`, `recommended`, `family`, `tiers`, `license`, `revision`, `update_available` | yes |
| `models.install` | `ModelInstallRequest`: `{id}` | `null`, then `models.progress` notifications | yes |
| `egress.recent` | `EgressRecentRequest`: `{limit}` (at most 1000) | list of `EgressEvent`: `at`, `host`, `feature`, `bytes_sent`, `bytes_received`, `items` | yes |
| `runtime.status` | `{}` | list of `RuntimeInfo`: `model_id`, `role` (`chat` or `embedding`), `state`, `provider`, `error`. Empty when `[runtime]` is disabled | yes |

`RuntimeInfo.state` is one of `not_installed`, `stopped`, `starting`, `ready`, `failed`.

### Chat

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `chat` | `ChatRequest`: `provider` (or `null` to route by tier), `tier`, `messages` (each `{role, text}`, role `system`, `user` or `assistant`; the last must be `user`), `items` (item refs whose content the messages include, for the egress ledger; at most 1024) | `ChatStarted`: `{stream_id, provider}`, then `chat.delta` and `chat.done` | yes |
| `chat.cancel` | `ChatCancel`: `{stream_id}` | `null` | yes |

When routing by tier, `ChatStarted` arrives after the first token, so that `provider` names the
provider that actually answered after any fallback.

### Search

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `search` | `SearchRequest`: `{query, limit}` (`limit` at most 200) | list of `SearchHit`: `item`, `chunk_id`, `title`, `snippet`, `score`, `url` | yes |

### Retrieval

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `retrieve.context` | `ContextRequest`: `{query, budget_tokens}` (budget at most 131072) | `ContextResponse`: `text` (passages, each under a `[n]` header), `citations`, `excluded_sensitive` | yes |
| `retrieve.ask` | `AskRequest`: `query`, `budget_tokens`, `provider` (optional; used alone, never substituted), `tier` (default `synthesis`) | `AskStarted`: `stream_id`, `provider`, `locality`, `citations`, `excluded_sensitive`; then `chat.delta` and `chat.done` | yes |

A `Citation` is `{n, item, chunk_id, title, url, sensitive}`. For `retrieve.ask` the daemon chooses
the provider first, then packs the context for it: with a `cloud` provider, sensitive items are left out
and counted in `excluded_sensitive`, and the context is repacked if it falls back to another provider.

### Sources and ingest

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `sources.list` | `{}` | list of `SourceInfo` | yes |
| `sources.add` | `SourceSpec`: `id` (optional; chosen by the daemon, for example `files-1`), `kind`, `name`, `settings` | `SourceInfo` | yes |
| `sources.update` | `SourceUpdate`: `id`, and any of `name`, `enabled`, `settings` | `SourceInfo` | yes |
| `sources.remove` | `SourceRemove`: `id`, `purge` (default `true`: also delete everything indexed from it) | `null` | yes |
| `sources.sync` | `SourceSync`: `{id}`, or `{}` for every enabled source | `null`; progress arrives as `ingest.progress` | yes |
| `sources.auth.begin` | `SourceRef`: `{id}` | `AuthBegin`: `{url}`. The client opens the URL; completion arrives as `sources.changed` | yes |
| `sources.secret.set` | `SourceSecret`: `{id, secret}` | `null` | yes |
| `ingest.status` | `{}` | `IngestStatus`: `queue_depth`, `freshness_p50_ms`, `freshness_p95_ms`, `embedding_provider`, `triage_provider`, `sources` | yes |

`SourceInfo` is `{id, kind, name, enabled, state, items, chunks, last_sync, last_error, settings}`, with
`state` one of `idle`, `syncing`, `watching`, `needs_auth`, `disabled`, `error`. Settings never
contain secrets: `sources.add` and `sources.update` refuse a `client_secret` key. Use
`sources.secret.set`: for `imap` and `caldav` it stores the password, for `gmail` and `gcal` the
OAuth client secret (then rebuilds the source). It is write-only, at most 4096 bytes. See
[Connector settings](connectors.md) for each kind's `settings`.

### Knowledge graph

Entity kinds: `person`, `organization`, `project`, `commitment`, `preference`, `event`, `place`,
`topic`. Claim statuses: `active`, `superseded`, `retracted` (lost its evidence), `rejected` (the user rejected it; sticky across re-extraction), `user_asserted`.

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `knowledge.entities` | `EntitiesRequest`: `text`, `kind`, `limit`, `offset` | list of `EntitySummaryView`: `id`, `kind`, `name`, `claims`, `mentions`, `updated_at` | yes |
| `knowledge.entity` | `EntityRef`: `{id}` | `EntityDetail`: `id`, `kind`, `name`, `aliases`, `identifiers`, `attributes`, `claims`, `mentions`, `vault_path` | yes |
| `knowledge.graph` | `GraphRequest`: `center` (or `null` for an overview), `depth`, `limit`, `kinds`, `source` (optional source filter) | `GraphViewIpc`: `nodes`, `edges`, `truncated` | yes |
| `knowledge.merge` | `MergeRequest`: `{into, from}` | `null` | yes |
| `knowledge.split` | `SplitRequest`: `{entity, claims, name, kind?, aliases?, identifiers?}` (moves the listed facts, aliases and identifiers to a new entity) | `EntityRef` of the new entity | yes |
| `knowledge.claim.status` | `ClaimStatusRequest`: `{claim, status}`, status `rejected` (sticky reject; `retracted` is an older spelling), `active` (restore), `user_asserted` or `unasserted` (undo the user's assertion) | `null` | yes |
| `knowledge.status` | `{}` | `KnowledgeStatus`: `entities`, `claims`, `mentions`, `pending_items`, `last_run`, `extractor_provider`, `vault` | yes |

### MCP clients

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `mcp.clients.list` | `{}` | list of `McpClientInfo`: `id`, `name`, `scopes`, `created_at`, `last_used` | yes |
| `mcp.clients.create` | `McpClientCreate`: `{name, scopes}` | `McpClientCreated`: `client`, `token` (returned once), `setup` (list of `McpSetupSnippet`: `app`, `config_path`, `text`) | yes |
| `mcp.clients.revoke` | `McpClientRef`: `{id}` | `null` | yes |

`McpScopes` is described in [MCP tools and scopes](mcp.md#scopes). The stdio bridge
(`nuggetd mcp --client <id>`) also talks to the daemon over this socket, with internal
`mcp.bridge.*` methods that are not part of the public protocol.

### Drafting (Double Tap, milestone M3)

Status: being wired in (wave W, Double Tap). The contracts below are already in `nugget_core::ipc`.

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `draft.begin` | `DraftRequest`: `context` (`DraftContext`: `app`, `field`, `thread`, `recipients`, `subject`, `screenshot_png_base64`), `instruction`, `provider`, `max_tokens` | `DraftStarted`: `stream_id`, `provider`, `locality`, `field_kind`, `citations`, `excluded_sensitive`; then `chat.delta` and `chat.done` | yes |
| `draft.cancel` | `ChatCancel`: `{stream_id}` | `null` | yes |
| `draft.feedback` | `DraftFeedback`: `stream_id`, `outcome` (`accepted`, `edited`, `rejected`), `final_text`, `insert_verified` | `null` | yes |
| `draft.prewarm` | A partial `DraftContext` (as much as is known when the trigger fires) | `{provider, locality, retrieval}`, or `null` when drafting is refused or disabled. Starts provider warm-up, retrieval and the voice lookup before `draft.begin` | yes |
| `voice.status` | `{}` | `VoiceStatus`: `samples`, `channels`, `last_built` | yes |

A field with `secure: true` (a password field) is refused, and its value is never read.

### Agent and safety (milestone M4)

Status: being wired in (wave W). The names are planned; parameters and results are not final.

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `agent.run` | A goal, the tools it may use, budgets | A task id; progress arrives as `agent.event` notifications | yes |
| `agent.cancel` | A task id | Stops a running task | yes |
| `agent.confirm` | A task id and an answer | Answers a pending confirmation (approve once, for this task, or deny) | yes |
| `agent.list` | Filters | Past and running tasks, from the action log | yes |
| `agent.get` | A task id | One task with its steps and an explanation of what it did | yes |
| `agent.answer` | `AgentAnswer`: a task id and the answer text | `null`; resumes a run waiting on `ask_user` | yes |
| `policy.get` | `{}` | The safety rules per app and site | yes |
| `policy.set` | The rules | Replaces the safety rules | yes |
| `policy.approvals` | `{}` | Standing approvals | yes |
| `policy.kill` | `{}` | Stops every agent now (the kill switch) | yes |
| `policy.resume` | `{}` | Allows agents to run again after a kill | yes |
| `policy.log.export` | `{}` | `PolicyLogExport`: the hash-chained action log as JSONL, with its chain status | yes |
| `browser.host.attach` | Host registration | Long-lived call from the browser extension's native host; returns when the daemon detaches it | yes |
| `browser.host.deliver` | An extension message | A reply or event from the extension, delivered by the native host | yes |
| `browser.status` | `{}` | `BrowserStatus`: whether the extension is connected, its origin and host version, and the browser tools registered for the agent | yes |

### Proactive cards and brief (milestone M5)

Status: being wired in (wave W). The names are planned; parameters and results are not final.

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `cards.list` | Filters | Current suggestion cards | in progress |
| `cards.feedback` | A card id and `accept`, `dismiss` or `snooze` | Records what you did with a card | in progress |
| `cards.preview` | A card id | A dry run of the card's action, with a digest the approval is bound to | in progress |
| `cards.execute` | A card id and the preview's digest | Runs the action, through the safety policy | in progress |
| `brief.get` | `{}` | Today's brief | in progress |
| `proactive.metrics` | `{}` | Acceptance and dismissal counts | in progress |

### Plugins (milestone M6)

Status: being wired in (wave W). The names are planned; parameters and results are not final.

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `plugins.list` | `{}` | Installed plugins | yes |
| `plugins.inspect` | A plugin | Its manifest, the capabilities it asks for and its signature status | yes |
| `plugins.install` | A local `.nuggetplugin` path | The capabilities to approve | yes |
| `plugins.approve` | A plugin and its capabilities | Approves them for this plugin version | yes |
| `plugins.remove` | A plugin | Uninstalls it | yes |
| `plugins.enable` | A plugin and on or off | Turns it on or off | yes |

## Notifications

<!-- check-refs: ipc.notifications -->
| Notification | Params | Sent when |
|---|---|---|
| `chat.delta` | `ChatDelta`: `{stream_id, text}` | A chat, Ask or draft stream produced text |
| `chat.done` | `ChatDone`: `{stream_id, error, input_tokens, output_tokens}`; `error` is `null` on success | The stream ended, failed or was cancelled |
| `models.progress` | `ModelProgress`: `{id, downloaded, total, phase, error}`, `phase` one of `downloading`, `verifying`, `installed`, `failed` | A model install progressed |
| `ingest.progress` | `IngestProgress`: `{source, phase, done, total, error}`, `phase` one of `scanning`, `indexing`, `idle`, `error` | A source's sync progressed |
| `sources.changed` | A `SourceInfo`; or `{"id": …, "removed": true}`; or `{"lagged": n}` | A source was added, changed (including after sign-in) or removed. Treat anything but a `SourceInfo` as "refetch `sources.list`" |
| `knowledge.changed` | `KnowledgeChanged`: `{entities}` | Entities changed (after an extraction run, a merge, a claim status change or a vault edit) |

Being wired in (wave W); the names are planned and the parameters not final:

<!-- check-refs: ipc.notifications -->
| Notification | Params | Sent when | Daemon |
|---|---|---|---|
| `agent.event` | An `AgentEvent` | An agent task took a step, needs a confirmation or finished | yes |
| `policy.kill.changed` | Whether the kill switch is engaged | The kill switch was engaged or released, from any cause (hotkey, tray, IPC) | yes |
| `cards.changed` | Not final | The set of suggestion cards changed | in progress |
| `browser.host.request` | A message for the extension | Daemon to native host: forward this request to the browser extension | yes |

## Errors

Errors use JSON-RPC error objects, `{code, message, data?}`. The `message` is meant for people; branch
on `code`.

<!-- check-refs: ipc.errors -->
| Name | Code | Meaning |
|---|---|---|
| `PARSE_ERROR` | `-32700` | The line was not valid JSON |
| `INVALID_REQUEST` | `-32600` | Not a valid JSON-RPC request |
| `METHOD_NOT_FOUND` | `-32601` | Unknown method, or one this daemon does not implement yet |
| `INVALID_PARAMS` | `-32602` | Parameters failed to parse or validate |
| `INTERNAL_ERROR` | `-32603` | The handler failed unexpectedly |
| `SERVER_ERROR` | `-32000` | Other server errors (for example "already being installed", "shutting down") |
| `NOT_FOUND` | `-32001` | No such source, provider, model or stream |
| `UNSUPPORTED` | `-32002` | The provider or connector cannot do this |
| `PROVIDER` | `-32003` | The model provider failed |
| `STORAGE` | `-32004` | The store failed |
| `INTEGRITY` | `-32005` | A verification failed (manifest, signature, hash) |
| `CANCELLED` | `-32006` | The operation was cancelled |
| `IO` | `-32007` | A file or network I/O error |
| `MESSAGE_TOO_LARGE` | `-32008` | A message exceeded 16 MiB |
| `UNAUTHORIZED` | `-32009` | A source or provider needs sign-in or a credential |

## Examples

Lines starting with `→` are sent by the client, `←` by the daemon (the arrows are not part of the
wire format).

### Status

```json
→ {"jsonrpc":"2.0","id":1,"method":"status","params":{}}
← {"jsonrpc":"2.0","id":1,"result":{"protocol":1,"version":"0.0.1","uptime_secs":42,"items":1432,"chunks":5210,"providers":[{"id":"local","kind":"openai_compatible","model":"qwen3:4b","locality":"on_device","tiers":["triage","light","synthesis"],"capabilities":null,"error":null}],"audit_chain_ok":true}}
```

### A streamed Ask

```json
→ {"jsonrpc":"2.0","id":2,"method":"retrieve.ask","params":{"query":"When is the Acme renewal due?","budget_tokens":4000}}
← {"jsonrpc":"2.0","id":2,"result":{"stream_id":"s-7","provider":"local-chat","locality":"on_device","citations":[{"n":1,"item":{"source":"gmail-1","key":"18c2f…"},"chunk_id":981,"title":"Renewal quote","url":"https://mail.google.com/mail/u/0/#all/18c2f…","sensitive":false}],"excluded_sensitive":0}}
← {"jsonrpc":"2.0","method":"chat.delta","params":{"stream_id":"s-7","text":"The renewal is due on 14 November"}}
← {"jsonrpc":"2.0","method":"chat.delta","params":{"stream_id":"s-7","text":" [1]."}}
← {"jsonrpc":"2.0","method":"chat.done","params":{"stream_id":"s-7","error":null,"input_tokens":1210,"output_tokens":14}}
```

### Adding a folder

```json
→ {"jsonrpc":"2.0","id":3,"method":"sources.add","params":{"kind":"files","name":"Documents","settings":{"roots":["/home/you/Documents"],"exclude":["/archive"]}}}
← {"jsonrpc":"2.0","id":3,"result":{"id":"files-1","kind":"files","name":"Documents","enabled":true,"state":"syncing","items":0,"chunks":0,"last_sync":null,"last_error":null,"settings":{"roots":["/home/you/Documents"],"exclude":["/archive"]}}}
← {"jsonrpc":"2.0","method":"ingest.progress","params":{"source":"files-1","phase":"scanning","done":120,"total":null,"error":null}}
```

### An IMAP source and its password

```json
→ {"jsonrpc":"2.0","id":4,"method":"sources.add","params":{"kind":"imap","name":"Fastmail","settings":{"host":"imap.fastmail.com","username":"you@fastmail.com"}}}
← {"jsonrpc":"2.0","id":4,"result":{"id":"imap-1","kind":"imap","name":"Fastmail","enabled":true,"state":"needs_auth","items":0,"chunks":0,"last_sync":null,"last_error":null,"settings":{"host":"imap.fastmail.com","username":"you@fastmail.com"}}}
→ {"jsonrpc":"2.0","id":5,"method":"sources.secret.set","params":{"id":"imap-1","secret":"<app password>"}}
← {"jsonrpc":"2.0","id":5,"result":null}
→ {"jsonrpc":"2.0","id":6,"method":"sources.sync","params":{"id":"imap-1"}}
← {"jsonrpc":"2.0","id":6,"result":null}
```

### An error

```json
→ {"jsonrpc":"2.0","id":7,"method":"cards.list","params":{}}
← {"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"method not found: cards.list"}}
```

The exact values in these examples (ids, stream ids, message wording, the generated source id) are
illustrative.

## Using the Rust client

```rust,ignore
use nugget_core::ipc::{method, SearchHit, SearchRequest};

let paths = nugget_core::paths::Paths::discover()?;
let client = nugget_ipc::Client::connect(&paths).await?;
let hits: Vec<SearchHit> = client
    .call(method::SEARCH, &SearchRequest { query: "renewal".into(), limit: 10 })
    .await?;
```

`Client::notifications()` returns a stream of notifications for that connection.
