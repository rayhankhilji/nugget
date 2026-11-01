# IPC protocol

The desktop app, the CLI and other local programs talk to `nuggetd` with **JSON-RPC 2.0**, one JSON
object per line (UTF-8, at most 16 MiB, else `-32008`). Types are in `nugget_core::ipc`; the Rust
client is `nugget_ipc::Client`. Protocol version `1` (reported by `status`).

Transport: on macOS and Linux a Unix socket `<data>/ipc/nuggetd.sock` in a `0700` directory (or a short
private directory under `$XDG_RUNTIME_DIR` if that path is too long); on Windows a named pipe
`\\.\pipe\nugget-<user SID>-<hash>-nuggetd.sock` admitting only you and SYSTEM. No TCP; opening the
socket is the credential. `nuggetd status` prints the path.

**Conventions.** Requests carry an `id` and may run concurrently on one connection. Notifications (no
`id`) carry streams and events: `chat.delta`, `chat.done` and `models.progress` go to the requesting
connection; `ingest.progress`, `sources.changed` and `knowledge.changed` are broadcast. Methods without
parameters take `{}`. Timestamps are RFC 3339; ids are strings; an `ItemRef` is
`{"source": "<source id>", "key": "<item key>"}`. Tiers: `triage`, `light`, `synthesis`, `agentic`,
`embedding`, `rerank` (never routed). Localities: `on_device`, `local_network`, `cloud`.

The **Daemon** column: `yes` = served; `not yet` = in the protocol but the daemon answers `-32601`;
`in progress` = being wired in on another branch (shape not final).

### Status, providers and models
<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `status` | `{}` | `StatusResponse`: `protocol`, `version`, `uptime_secs`, `items`, `chunks`, `providers` (list of `ProviderInfo`), `audit_chain_ok`, `pid` (the daemon's process id), `service` (`systemd` or `launchd` when a login service runs it; absent otherwise) | yes |
| `daemon.shutdown` | `DaemonShutdownRequest`: `{reason?}` (for the log) | `DaemonShutdownResponse`: `{stopping, pid}`. Sent before the daemon winds down as on SIGTERM (ingest, local model servers, store) and exits 0. Used by the desktop app when it quits or replaces a daemon of another version. Only the local socket serves it; MCP, HTTP and the browser extension never forward it | yes |
| `providers.list` | `{}` | list of `ProviderInfo`: `id`, `kind`, `model`, `locality`, `tiers`, `capabilities` (`null` until probed), `error` (why it is unavailable) | yes |
| `providers.probe` | `ProbeRequest`: `{provider}` | `Capabilities`: `streaming`, `vision`, `tools`, `json_schema`, `embeddings`, `context_tokens` | yes |
| `models.list` | `{}` | list of `ModelInfo`: `id`, `size_bytes`, `installed`, `recommended`, `family`, `tiers`, `license`, `revision`, `update_available` | yes |
| `models.install` | `ModelInstallRequest`: `{id}` | `null`, then `models.progress` notifications | yes |
| `models.cancel` | `ModelCancelRequest`: `{id}` | `{cancelled}` (`false` if no install of `id` was running); the install's connection gets a final `failed` progress with reason `cancelled`, and the staged bytes are kept so the next `models.install` resumes | yes |
| `egress.recent` | `EgressRecentRequest`: `{limit}` (at most 1000) | list of `EgressEvent`: `at`, `host`, `feature`, `bytes_sent`, `bytes_received`, `items` | yes |
| `egress.page` | `EgressPageRequest`: `{limit, cursor}` (`limit` at most 1000; `cursor` optional: the previous page's `next_cursor`) | `{items, next_cursor}`: `items` a list of `EgressEvent`, newest first; `next_cursor` (a string, or `null` after the oldest row) continues after the last of them. Keyset paging on the ledger row: rows recorded while you page never shift later pages, so none is skipped or listed twice. Cursors are opaque; one this daemon did not return is invalid params (`-32602`) | yes |
| `egress.record` | `EgressRecordRequest`: `host` (bare `host[:port]`), `feature` (`update_check` or `update_download` only), `bytes_sent`, `bytes_received` (default 0), `at` (RFC 3339, default now; at most 5 minutes ahead and 30 days old; stored in UTC) | `null`. A request the desktop app made itself, written to the ledger with no items. Host: a DNS name, an IPv4 address or a bracketed IPv6 address, optional port. At most 60 rows a minute across all clients (`SERVER_BUSY` beyond) | yes |
| `runtime.status` | `{}` | list of `RuntimeInfo`: `model_id`, `role` (`chat`, `embedding`, `rerank` or `draft`), `state` (`not_installed`, `stopped`, `starting`, `ready`, `failed`), `provider`, `error` (one plain sentence), and when present `state_ms` (time in this state: the warm-up so far while `starting`), `expected_ms` (expected load time while `starting`), `retry_in_ms` (next automatic restart while `failed`), `gave_up` (`failed` and waiting for `runtime.retry`), `detail` (the server's last line). Empty when `[runtime]` is disabled | yes |
| `runtime.retry` | `RuntimeRetryRequest`: `{role}` | `{started}`: clears a failed start or a give-up and starts the role's server in the background (`false` when its model is not installed) | yes |

### Search
<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `search` | `SearchRequest`: `{query, limit}` (`limit` at most 200) | list of `SearchHit`: `item`, `chunk_id`, `title`, `snippet`, `score`, `url` | yes |
| `search.page` | `SearchPageRequest`: `{query, limit, cursor}` (`limit` at most 200; `cursor` optional: the previous page's `next_cursor` for the same `query`) | `{items, next_cursor}`: `items` a list of `SearchHit` (as `search`), `next_cursor` a string, or `null` at the end. Every page ranks the query to the same depth (500 hits, where paging stops) and returns its slice, so pages neither repeat nor skip a hit unless items are indexed between two pages; merge pages by item and chunk. Cursors are opaque and tied to their query; any other is invalid params (`-32602`) | yes |
| `search.faceted` | `{query, limit, offset, filters, utc_offset_minutes, time_zone}`: `filters` has `sources`, `kinds` (`mail`, `event`, `chat`, `file`), `people`, `from`, `to`; `limit` at most 100 | Hits (each with its item date, kind, `sensitive` and people) and facet counts for `sources`, `kinds`, `people` and `months` | yes |
| `search.timeline` | `{entity, from, to, limit, utc_offset_minutes, time_zone}` (`limit` at most 1000) | The entity's items and active facts in time order (the newest `limit`), month counts and `truncated` | yes |
| `search.preview` | `{item, chunk_id, utc_offset_minutes, time_zone}` | The item's text (at most 20,000 characters), the matched passage, its people and its facts with quotes | yes |
| `search.upgrade.status` | `{}` | `SearchUpgradeStatus`: `offer` (`{from, to, download_bytes}`, only while the index's on-device embedding model is not the recommended one, no model is pinned in config and the offer was neither accepted nor dismissed) and `job` (`{to, phase, done, total, error}`: `phase` `downloading` counts bytes, `embedding` counts chunks), each `null` when absent | yes |
| `search.upgrade.start` | `{}` | `SearchUpgradeStatus`. Accepts the offer (or retries one that stopped on an error): downloads the model if needed, then re-embeds every chunk in the background at the lowest priority (not on low power, after interactive model use), resuming across restarts. Searches use the old vectors until every chunk has a new one; then the index switches in one transaction and the search model restarts on the new model. Invalid params when nothing is on offer | yes |
| `search.upgrade.dismiss` | `{}` | `SearchUpgradeStatus`. Hides the offer for good and cancels an upgrade in progress (the old index stays) | yes |

`time_zone` is an IANA name; without it `utc_offset_minutes` applies, then `[retrieve]
date_utc_offset_minutes`.

### Retrieval
<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `retrieve.context` | `ContextRequest`: `{query, budget_tokens}` (budget at most 131072) | `ContextResponse`: `text` (passages, each under a `[n]` header), `citations`, `excluded_sensitive` | yes |
| `retrieve.ask` | `AskRequest`: `query`, `budget_tokens`, `provider` (optional; used alone, never substituted), `tier` (default `synthesis`), `history` (optional earlier `ChatMessage`s, oldest first) | `AskStarted`: `stream_id`, `provider`, `locality`, `citations`, `excluded_sensitive`, `query_rewrite` (when a follow-up was rewritten), `general`, `date` (when the question named a date), `partial` (citation numbers of near misses); then `chat.delta` and `chat.done` | yes |
| `chat` | `ChatRequest`: `provider` (or `null` to route by tier), `tier`, `messages` (each `{role, text}`, role `system`, `user` or `assistant`; the last must be `user`), `items` (item refs whose content the messages include, for the egress ledger; at most 1024) | `ChatStarted`: `{stream_id, provider}`, then `chat.delta` and `chat.done` | yes |
| `chat.cancel` | `ChatCancel`: `{stream_id}` | `null` | yes |

`chat` routes by tier unless `provider` is set; `ChatStarted` arrives after the first token, naming the
provider that answered. A `Citation` is `{n, item, chunk_id, title, url, sensitive}`. The provider is chosen first; with a
`cloud` provider sensitive items are left out (counted in `excluded_sensitive`), and the context is
repacked on fallback. With `history`, a follow-up is rewritten into a standalone query by a local
model (or a heuristic). `AskStarted.date` is `{phrase, start, end, applied, listed}` for a date phrase
in the question; `partial` lists citations that are near misses. Both are absent when empty.

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

`SourceInfo` is `{id, kind, name, enabled, state, items, chunks, last_sync, last_error, settings}`;
`state` is `idle`, `syncing`, `watching`, `needs_auth`, `disabled` or `error`. Secrets go through the
write-only `sources.secret.set` (at most 4096 bytes), never `settings`. See
[Connector settings](connectors.md).

### Knowledge graph

Entity kinds: `person`, `organization`, `project`, `commitment`, `preference`, `event`, `place`,
`topic`. Claim statuses: `active`, `superseded`, `retracted` (lost its evidence), `rejected` (the user rejected it; sticky across re-extraction), `user_asserted`.

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `knowledge.entities` | `EntitiesRequest`: `text`, `kind`, `limit`, `offset` | list of `EntitySummaryView`: `id`, `kind`, `name`, `claims`, `mentions`, `updated_at` | yes |
| `knowledge.entity` | `EntityRef`: `{id}` | `EntityDetail`: `id`, `kind`, `name`, `aliases`, `identifiers`, `attributes`, `claims` (each with `sensitive`), `mentions`, `vault_path`, `sensitive` (known only from sensitive items) | yes |
| `knowledge.graph` | `GraphRequest`: `center` (or `null` for an overview), `depth`, `limit`, `kinds`, `source` (optional source filter), `include_sensitive` (default `false`: entities and facts known only from sensitive items are left out, except the centre) | `GraphViewIpc`: `nodes` (each with `sensitive`), `edges`, `truncated` | yes |
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

`McpScopes`: see [MCP tools and scopes](mcp.md#scopes). The stdio bridge uses internal, non-public
bridge methods on this socket.

### Drafting (Double Tap)
<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `draft.begin` | `DraftRequest`: `context` (`DraftContext`: `app`, `field`, `thread`, `recipients`, `subject`, `screenshot_png_base64`), `instruction`, `provider`, `max_tokens` | `DraftStarted`: `stream_id`, `provider`, `locality`, `field_kind`, `citations`, `excluded_sensitive`; then `chat.delta` and `chat.done` | yes |
| `draft.cancel` | `ChatCancel`: `{stream_id}` | `null` | yes |
| `draft.feedback` | `DraftFeedback`: `stream_id`, `outcome` (`accepted`, `edited`, `rejected`), `final_text`, `insert_verified` | `null` | yes |
| `draft.prewarm` | A partial `DraftContext` (as much as is known when the trigger fires) | `{provider, locality, retrieval}`, or `null` when drafting is refused or disabled. Starts provider warm-up, retrieval and the voice lookup before `draft.begin` | yes |
| `draft.read_ahead` | `DraftRequest`, as for `draft.begin` (the context read while the field has focus) | Answers within about 100 ms. `{"started": true}` while the prompt is being prefilled into a non-cloud server's cache in the background, `{"started": true, "done": true}` if it already finished, or `{"started": false, "skipped": reason}` with `disabled`, `refused`, `cloud`, `rate_limited`, `unchanged`, `unavailable` or `cancelled`. No text is generated, nothing is sent when the draft would go to a cloud provider, and nothing is audited or stored | yes |
| `draft.read_ahead_cancel` | `{}` | `{cancelled}`: whether a read-ahead was in flight. Sent when focus leaves the field | yes |
| `draft.check_sensitive` | `{text}` | `{sensitive}`: whether the ingest PII scan (cards, IBANs, SSNs, API keys and other credentials) finds anything; text over 64 KiB is sensitive. The desktop asks before keeping a draft in its history; nothing is logged | yes |
| `voice.status` | `{}` | `VoiceStatus`: `samples`, `channels`, `last_built` | yes |

A field with `secure: true` (a password field) is refused, and its value is never read.

### Agent, safety and plugins
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
| `plugins.list` | `{}` | Installed plugins | yes |
| `plugins.inspect` | A plugin | Its manifest, the capabilities it asks for and its signature status | yes |
| `plugins.install` | A local `.nuggetplugin` path | The capabilities to approve | yes |
| `plugins.approve` | A plugin and its capabilities | Approves them for this plugin version | yes |
| `plugins.remove` | A plugin | Uninstalls it | yes |
| `plugins.enable` | A plugin and on or off | Turns it on or off | yes |
| `plugins.catalog` | `{}` | Signed packages found in the catalog folders, verified but not installed, with their trust, requested access and whether they update an installed plugin | yes |

### Proactive cards and brief

An approval is bound to the exact preview (a keyed digest, valid once); every action passes the safety
policy.

Card, brief and action text comes in English (`title`, `detail`, `summary`, `label`) and as a
`LocalText` beside it (`title_text`, `detail_text`, `summary_text`, `label_text`, and a
`needs_confirmation` result's `detail_text`): a message key and raw arguments (ISO dates, numbers,
an amount with its currency) the app formats in the user's language. Unknown keys fall back to the
English.

<!-- check-refs: ipc.methods -->
| Method | Params | Result | Daemon |
|---|---|---|---|
| `cards.list` | `CardsListRequest`: `{include_closed}` | `CardsList`: the open cards best first, each with its top factors, evidence, offered actions and `learned` (how your feedback changed its ranking: scope, key, `less`/`more`/`hidden`, the accepts and dismissals behind it) | yes |
| `cards.feedback` | `CardFeedbackRequest`: a card id and `accept`, `dismiss` (with a reason), `snooze` (until), `seen` or `done` | The updated `CardView` | yes |
| `cards.preview` | `CardPreviewRequest`: a card id and an action index | `CardPreview`: a dry-run diff and the digest an approval is bound to | yes |
| `cards.execute` | `CardExecuteRequest`: card id, action index, the preview's digest, `confirmed` | `CardExecuteResult`: `executed` (with an artifact for the app to finish), `needs_confirmation`, `denied` or `halted` | yes |
| `brief.get` | `{}` | `BriefView`: today's brief | yes |
| `proactive.metrics` | `{}` | `ProactiveMetrics`: precision, time to action, dismiss reasons | yes |
| `proactive.refresh` | `{}` | `CardsChanged`: runs a refresh now | yes |
| `reminders.list` | `{}` | Pending `ReminderView`s, soonest first | yes |
| `proactive.learning` | `{}` | `ProactiveLearning`: what feedback taught the ranking, per sender, domain, kind and thread, strongest adjustment first (answers even when `[proactive] enabled = false`) | yes |
| `proactive.reset_learning` | `ResetLearningRequest`: `{scope, key}` with `scope` `all`, `sender`, `domain`, `kind` (key: a card kind) or `thread`; `key` required unless `all` | `ResetLearningResult`: `{cleared}`. Recorded in the audit log (`proactive.reset_learning`, target `all` or `<scope>:<key>`) and broadcasts `cards.changed` with reason `reset_learning` | yes |

## Notifications
<!-- check-refs: ipc.notifications -->
| Notification | Params | Sent when |
|---|---|---|
| `chat.delta` | `ChatDelta`: `{stream_id, text}` | A chat, Ask or draft stream produced text |
| `chat.done` | `ChatDone`: `{stream_id, error, input_tokens, output_tokens}`; `error` is `null` on success | The stream ended, failed or was cancelled |
| `ask.restarted` | `AskRestarted`: `{stream_id, provider, locality, citations, excluded_sensitive, general, date, partial}` | An Ask fell back to another provider before its first delta: replace the sources shown so far with these citations; deltas of the new provider follow |
| `models.progress` | `ModelProgress`: `{id, downloaded, total, phase, error, reason?, http_status?}`, `phase` one of `downloading`, `verifying`, `installed`, `failed`. In `failed`, `downloaded` is the bytes staged for a resume, `error` a short plain-language message, and `reason` one of `offline`, `http_error` (with `http_status`), `captive_portal`, `stalled`, `interrupted`, `disk_full`, `integrity`, `cancelled`, `other` | A model install progressed |
| `ingest.progress` | `IngestProgress`: `{source, phase, done, total, error}`, `phase` one of `scanning`, `indexing`, `idle`, `error` | A source's sync progressed |
| `sources.changed` | A `SourceInfo`; or `{"id": …, "removed": true}`; or `{"lagged": n}` | A source was added, changed (including after sign-in) or removed. Treat anything but a `SourceInfo` as "refetch `sources.list`" |
| `knowledge.changed` | `KnowledgeChanged`: `{entities}` | Entities changed (after an extraction run, a merge, a claim status change or a vault edit) |
| `agent.event` | An `AgentEvent` | An agent task took a step, needs a confirmation or finished |
| `policy.kill.changed` | Whether the kill switch is engaged | The kill switch was engaged or released, from any cause (hotkey, tray, IPC) |
| `cards.changed` | `CardsChanged`: `{reason, open, created, resolved}` | The set of suggestion cards changed (refresh, feedback, an executed action or a learning reset: `reset_learning`); refetch `cards.list` |
| `brief.ready` | A `BriefView` | The morning brief is ready, once a day at `[proactive] brief_time` (not sent when it is empty) |
| `reminder.due` | A `ReminderView` | A reminder the user approved is due; the app shows an OS notification |
| `browser.host.request` | A message for the extension | Daemon to native host: forward this request to the browser extension |

## Errors

Errors are `{code, message, data?}`; branch on `code`, not `message`.

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
| `SERVER_BUSY` | `-32010` | The connection already has its maximum number of requests in flight (`data`: `{"max_in_flight": n}`); retry once an earlier call on the connection has finished. Control methods such as cancel and the kill switch have reserved slots and never get this. |
