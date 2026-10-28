# config.toml

`nuggetd` reads `config.toml` from the config directory (see [Data directory](data-dir.md)) once, at
startup. Restart the daemon after editing it. If the file does not exist, the daemon writes a commented,
local-first default there (readable only by you) and says so.

**Never put secrets in this file.** API keys and credential-bearing headers must be references:
`env:VAR` (an environment variable of the daemon) or `keychain:NAME` (an OS keychain entry, service
`dev.nugget.Nugget`, account `provider:NAME`). A plaintext key is rejected.

**Validation.** Unknown sections and unknown keys in `[models]`, `[models.selected]`, `[runtime]`,
`[mcp]` and `[[providers]]` are errors, as are out-of-range values, duplicate provider ids and
malformed model ids. Unknown keys inside `[ingest]`, `[retrieve]`, `[knowledge]` and `[vault]` are
currently **ignored**, not rejected, so check their spelling against this page.

<!-- check-refs: config.sections -->
| Section | What it configures |
|---|---|
| `models` | Model downloads, and the local model chosen per tier |
| `runtime` | The on-device `llama-server` runtime that `nuggetd` manages |
| `ingest` | Indexing: extraction, chunking, triage, embeddings, scheduling |
| `retrieve` | Search and context packing |
| `knowledge` | The knowledge graph: extraction runs, who you are, the optional LLM extractor, entity resolution |
| `vault` | The Markdown vault the knowledge graph is rendered into |
| `mcp` | The MCP server for AI apps: stdio bridge and loopback HTTP |
| `providers` | Model providers, as an array of tables (`[[providers]]`) |
| `draft` | Double Tap drafting and the voice profile. Owned by a daemon feature; see [Feature sections](#feature-sections) |
| `agent` | The agent runtime and computer use. Feature section |
| `policy` | The safety policy for agent actions. Feature section |
| `browser` | The browser extension bridge. Feature section |
| `proactive` | Proactive cards and the morning brief. Feature section |
| `plugins` | WebAssembly plugins. Feature section |

Every key below is optional; an empty file is valid and gives the defaults (with no providers).

## `[models]`

<!-- check-refs: config.models -->
| Key | Type | Default | Description |
|---|---|---|---|
| `download_parts` | integer, 1 to 16 | `4` | Parallel connections per model download |
| `selected` | table | empty | The `[models.selected]` table below |

### `[models.selected]`

The local model chosen for each tier, by manifest id (as listed on the Models page or by
`models.list`). A model id is 1 to 64 characters of lowercase letters, digits, `.`, `_` and `-`.

<!-- check-refs: config.models.selected -->
| Key | Type | Default | Description |
|---|---|---|---|
| `triage` | model id | unset | Model for the triage tier; also the runtime's chat model when `runtime.chat_model` is unset |
| `light` | model id | unset | Model for the light tier |
| `synthesis` | model id | unset | Model for the synthesis tier |
| `agentic` | model id | unset | Model for the agentic tier |
| `embedding` | model id | unset | Model for embeddings; also the runtime's embedding model when `runtime.embedding_model` is unset |

## `[runtime]`

The on-device runtime: `nuggetd` runs installed models with a pinned, verified `llama-server` on
`127.0.0.1`, as the providers `local-chat` (triage, light, synthesis) and `local-embedding`
(embedding).

<!-- check-refs: config.runtime -->
| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Run selected, installed models locally |
| `chat_model` | model id | unset | Chat model. Unset: `models.selected.triage`, then the manifest's recommendation for this machine (triage tier, else synthesis) |
| `embedding_model` | model id | unset | Embedding model. Unset: `models.selected.embedding`, then the recommendation |
| `idle_stop_minutes` | integer, 0 to 10080 | `30` | Stop a server after this many idle minutes; `0` keeps it running. It restarts on the next request |

## `[ingest]`

Tunables for the indexing pipeline (`nugget_ingest::IngestConfig`). Durations are in milliseconds.

<!-- check-refs: config.ingest -->
| Key | Type | Default | Description |
|---|---|---|---|
| `extract_timeout_ms` | integer | `30000` | Per-item extraction timeout. On timeout the item is indexed by title only |
| `max_input_bytes` | integer | `67108864` | Inputs larger than this (64 MiB) are not extracted; the item is indexed by title only |
| `max_text_chars` | integer | `2000000` | Extracted text is cut after this many characters |
| `ocr` | bool | `true` | Run OCR on images, when the OCR models are present (see `NUGGET_OCR_MODELS`) |
| `chunk_target_tokens` | integer | `400` | Chunker target size, in estimated tokens |
| `chunk_max_tokens` | integer | `512` | Chunker hard maximum, in estimated tokens |
| `chunk_overlap_tokens` | integer | `48` | Overlap between chunks, in estimated tokens |
| `queue_capacity` | integer | `64` | Changes buffered per source before the connector waits |
| `keep_text` | bool | `true` | Keep each item's full extracted text (needed for previews and re-chunking) |
| `triage` | bool | `true` | Run triage when a local triage-tier provider with JSON-schema output exists. Cloud providers are never used |
| `triage_timeout_ms` | integer | `30000` | Timeout for one triage call |
| `triage_max_chars` | integer | `6000` | Characters of text sent to triage |
| `embeddings` | bool | `true` | Compute embeddings with embedding-tier providers |
| `allow_cloud_embeddings` | bool | `false` | Allow cloud embedding providers for non-sensitive items |
| `embed_batch` | integer | `32` | Chunks per embedding request |
| `embed_timeout_ms` | integer | `120000` | Timeout for one embedding request |
| `reembed_interval_ms` | integer | `600000` | How often to embed items that were indexed text-only; `0` means only at startup and when a local embedder becomes ready |
| `backoff_base_ms` | integer | `2000` | First retry delay after a failed sync pass; doubles per failure |
| `backoff_max_ms` | integer | `900000` | Retry delay cap (15 minutes) |
| `watch_resync_ms` | integer | `3600000` | While watching, re-run a reconciling sync this often; `0` disables |
| `low_battery_threshold` | float, 0 to 1 | `0.2` | Battery fraction under which backfills pause while discharging |
| `power_poll_ms` | integer | `60000` | How often the power state is re-checked while paused |
| `error_ring` | integer | `32` | Item-level errors kept per source |
| `max_concurrent_extractions` | integer | `0` | Extractions running at once across all sources; `0` means one per CPU |
| `freshness_window` | integer | `1024` | Samples kept for the freshness p50 and p95 |

## `[retrieve]`

Search and context packing (`nugget_retrieve::RetrieveConfig`).

<!-- check-refs: config.retrieve -->
| Key | Type | Default | Description |
|---|---|---|---|
| `allow_cloud_embedding` | bool | `false` | Send queries (and, for MMR, passages stored without a vector) to a cloud embedding provider. Off: a cloud-only embedding tier means text-only search |
| `overfetch` | integer | `8` | Candidate chunks fetched per requested result, before grouping by item |
| `min_candidates` | integer | `64` | At least this many candidate chunks |
| `max_candidates` | integer | `512` | At most this many candidate chunks |
| `fusion_depth_factor` | integer | `4` | Hybrid search fuses the top `max(limit × factor, min_fusion_depth)` hits of each list |
| `min_fusion_depth` | integer | `40` | Minimum hits per list in fusion |
| `mmr` | bool | `true` | Re-rank with maximal marginal relevance for diversity |
| `mmr_lambda` | float, 0 to 1 | `0.7` | MMR trade-off: 1.0 is pure relevance, 0.0 pure novelty |
| `mmr_pool_factor` | integer | `3` | Items MMR chooses from, as a multiple of the requested count |
| `mmr_pool_max` | integer | `64` | Upper bound on the MMR pool |
| `mmr_vectors` | bool | `true` | Compare passages by their stored vectors when the query was embedded; otherwise by word overlap |
| `mmr_embed_chars` | integer | `2000` | Characters of a passage embedded for MMR (only passages stored without a vector) |
| `recency` | table | unset | Optional recency boost, below |
| `max_matches_per_item` | integer | `8` | Other matching chunks remembered per item |
| `context_max_items` | integer | `10` | Most items (citations) packed into one context |
| `expand_neighbours` | bool | `true` | Add the chunks around each packed passage while the budget allows |
| `include_secondary_matches` | bool | `true` | Also pack an item's other matching chunks while the budget allows |
| `min_truncated_tokens` | integer | `24` | A passage that does not fit is cut to fit only if at least this many tokens remain |

### `[retrieve.recency]`

Off unless present. Multiplies an item's relevance by `1 + weight × 0.5^(age_days / half_life_days)`,
where the age comes from the item's modification time. When the table is present, both keys are
required.

<!-- check-refs: config.retrieve.recency -->
| Key | Type | Default when used | Description |
|---|---|---|---|
| `half_life_days` | float | `30` | Age at which the boost halves |
| `weight` | float | `0.2` | Size of the boost for a brand-new item |

## `[knowledge]`

The knowledge graph: people, organisations, projects and facts extracted from your indexed items,
every fact cited. Extraction runs after indexing settles, periodically and at start. Deterministic
extractors (mail headers, calendar attendees, contacts) always run; the LLM extractor uses a
synthesis-tier model on this device or your network, unless you list sources in `llm.cloud_sources`.
Sensitive items never go to a cloud model.

<!-- check-refs: config.knowledge -->
| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Run extraction at all |
| `interval_minutes` | integer, 0 to 10080 | `10` | Run again this often even without new items (retries LLM extraction); `0` never |
| `debounce_ms` | integer, 0 to 600000 | `1500` | Wait this long after the last indexed change before a run |
| `max_items_per_run` | integer, at least 1 | `500` | Items processed per run; a run that leaves items is followed by another one |
| `max_llm_requests_per_run` | integer | `50` | LLM extraction requests per run (none while on low battery) |
| `max_run_seconds` | integer | `120` | Wall-clock limit of one run; `0` none |
| `me` | table | empty | Who you are, below |
| `extra_freemail_domains` | array of strings | `[]` | Webmail domains added to the built-in list. Addresses there never produce organisations or `works_at` facts |
| `mail_works_at_confidence` | float, 0 to 1 | `0.5` | Confidence of a `works_at` fact inferred from a work mail domain |
| `max_correspondents` | integer | `20` | Messages with more participants than this link only the sender (mailing lists, announcements) |
| `single_valued_predicates` | array of strings | built-in list | Predicates that hold one value at a time, so a newer fact supersedes older ones. Replaces the built-in list when not empty |
| `llm` | table | see below | The optional LLM extractor |
| `resolution` | table | see below | Entity resolution beyond identifiers and exact names |
| `flush_every` | integer | `64` | Save work-queue progress at least every this many items |
| `max_chunks_per_item` | integer | `512` | Chunks read per item at most (longer items are cut for extraction) |

### `[knowledge.me]`

<!-- check-refs: config.knowledge.me -->
| Key | Type | Default | Description |
|---|---|---|---|
| `name` | string | unset | Your display name. Unset: inferred from sent mail, else `Me` |
| `emails` | array of strings | `[]` | Your own mail addresses, so mail and calendar facts hang off your own entity |
| `infer_from_sent` | bool | `true` | Learn further addresses from the `From` of mail in a sent folder |

### `[knowledge.llm]`

<!-- check-refs: config.knowledge.llm -->
| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Use the LLM extractor when a suitable model exists |
| `cloud_sources` | array of source ids | `[]` | Sources whose **non-sensitive** items may be sent to a cloud provider. Empty: local models only |
| `max_input_chars` | integer | `12000` | Characters of text per request |
| `max_requests_per_item` | integer | `4` | Requests per item; the rest of a long item is not sent |
| `max_output_tokens` | integer | `2048` | Output cap per request |
| `timeout_ms` | integer | `120000` | Timeout of one request |
| `max_confidence` | float, 0 to 1 | `0.85` | Upper bound on the confidence of an LLM-extracted fact |
| `max_attempts` | integer | `3` | Give up on an item after this many failures, until it changes |

### `[knowledge.resolution]`

Both steps are off by default and only ever use local models.

<!-- check-refs: config.knowledge.resolution -->
| Key | Type | Default | Description |
|---|---|---|---|
| `embedding` | bool | `false` | Also match entity names by embedding similarity |
| `embedding_threshold` | float, 0 to 1 | `0.95` | Similarity at or above which two names are the same entity |
| `embedding_kinds` | array of entity kinds | `["organization", "project", "topic", "place"]` | Kinds the embedding check applies to |
| `adjudicate` | bool | `false` | Ask a local synthesis-tier model about ambiguous pairs |
| `adjudicate_above` | float, 0 to 1 | `0.85` | Pairs with similarity from here up to `embedding_threshold` are ambiguous |
| `max_candidates` | integer | `20` | Candidates compared per entity at most |

## `[vault]`

One Markdown page per entity, with footnote citations, under `<Kind>/<Name> (<id>).md`. Your edits
are merged back: changed or added fact lines become facts you asserted. See
[Data directory](data-dir.md#the-vault).

<!-- check-refs: config.vault -->
| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Render and watch the vault |
| `path` | path | `~/Nugget Vault` | Vault directory, absolute or starting with `~`. With `--home` or `NUGGET_HOME` the default is `<data>/vault` |
| `include_identifiers` | bool | `false` | Put identifiers (mail addresses, phone numbers) in page frontmatter. Off because the vault is plain text that may be synced elsewhere |
| `max_name_chars` | integer | `80` | Longest entity-name part of a file name, in characters |
| `debounce` | integer, milliseconds | `750` | How long the watcher waits for a burst of edits to settle |

## `[mcp]`

The MCP server that lets AI apps read your knowledge with a per-app token. See
[Connecting AI apps over MCP](../guide/mcp.md).

<!-- check-refs: config.mcp -->
| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Serve MCP at all: clients, the stdio bridge and HTTP. Off: the `mcp.clients.*` methods fail |
| `http` | bool | `true` | Also listen for streamable HTTP on `127.0.0.1` |
| `port` | integer | `7319` | HTTP port on `127.0.0.1`; `0` picks a free port. If the port is taken, HTTP is skipped (with a warning) and stdio still works |
| `allowed_origins` | array of origins | `[]` | Exact `Origin` values allowed on HTTP requests, such as `http://localhost:6274` for the MCP Inspector. Empty: any request with an `Origin` header (a browser page) is refused |

## `[[providers]]`

One table per model provider. Tiers are tried in this order: on-device and local-network providers in
file order, then the managed runtime, then cloud providers in file order. See
[Local or bring-your-own-key models](../guide/models.md).

<!-- check-refs: config.providers -->
| Key | Type | Default | Description |
|---|---|---|---|
| `id` | string | required | Unique name, used in logs, the egress ledger and IPC |
| `kind` | `anthropic`, `openai_responses` or `openai_compatible` | required | Wire protocol: the Anthropic Messages API, the OpenAI Responses API, or OpenAI-compatible chat completions and embeddings (Ollama, LM Studio, `llama-server`, vLLM, OpenRouter) |
| `base_url` | URL | per kind | API root. `anthropic`: without `/v1`, default `https://api.anthropic.com`. `openai_responses`: default `https://api.openai.com/v1`. `openai_compatible`: required, including the version prefix, for example `http://127.0.0.1:11434/v1`. Plain `http` to an internet host is refused |
| `model` | string | required | The provider's model name |
| `api_key` | `env:VAR` or `keychain:NAME` | none | Omit for local servers that need no key |
| `tiers` | array of `triage`, `light`, `synthesis`, `agentic`, `embedding` | `[]` | Tiers this provider serves |
| `locality` | `on_device`, `local_network` or `cloud` | from the host | Treat the endpoint as further away than its address suggests (for a local gateway that forwards to the cloud). It can only raise the classification, never lower it |
| `context_tokens` | integer | unset | Context window, if known |
| `max_output_tokens` | integer | unset | Output cap when a request sets none. Anthropic requires one and falls back to 16000 |
| `connect_timeout_secs` | integer | `10` | TCP and TLS connect timeout |
| `idle_timeout_secs` | integer | `600` | Abort a response after this long without receiving bytes |
| `max_retries` | integer | `2` | Retries for transient failures before the stream starts |
| `retry_base_ms` | integer | `500` | Base backoff between retries, doubled per attempt |
| `headers` | table of strings | empty | Extra HTTP headers. Credential-bearing headers must use `env:` or `keychain:` references |
| `extra_body` | table | unset | JSON object merged into every chat request body |
| `anthropic_version` | string | `2026-10-21` | Anthropic only: the `anthropic-version` header |
| `eager_input_streaming` | bool | `true` | Anthropic only: stream tool inputs eagerly |
| `store` | bool | `false` | OpenAI Responses only: let OpenAI store responses server-side |
| `stream_usage` | bool | `true` | OpenAI-compatible only: ask for token usage in the stream. Turn off for servers that reject `stream_options` |
| `max_tokens_param` | `max_tokens` or `max_completion_tokens` | `max_tokens` | OpenAI-compatible only: the name of the output-cap field (OpenAI reasoning models need `max_completion_tokens`) |
| `embed_batch_size` | integer | `64` | OpenAI-compatible only: texts per embeddings request |
| `llama_cpp` | bool | `true` for on-device endpoints, else `false` | OpenAI-compatible only: the server is llama.cpp's `llama-server`. Requests then ask it to reuse the cached prompt prefix, and the stable prefix can be evaluated ahead of a request. Ollama, LM Studio and vLLM ignore the extra fields |
| `slot_affinity` | bool | `false` | OpenAI-compatible `llama-server` only: send requests that share a cache key to a fixed server slot. Off by default, because `llama-server` already picks the idle slot with the longest cached prefix, and a pinned request waits when its slot is busy |
| `socket_path` | string | unset | Unix only, OpenAI-compatible on-device servers: send every request to this Unix socket. `base_url` then supplies only the scheme, `Host` header and path prefix (for example `http://localhost/v1`). The on-device runtime uses this for `llama-server` by default |

### Locality

| Address | Locality | Egress ledger |
|---|---|---|
| Loopback (`127.0.0.1`, `::1`, `localhost`) | `on_device` | Not recorded: nothing leaves the machine |
| Private ranges (`10/8`, `172.16/12`, `192.168/16`, link-local, ...) | `local_network` | Recorded |
| Anything else | `cloud` | Recorded |

Host names are resolved once, when the provider is built, and the result is pinned.

## Feature sections

Status: being wired in (wave W). `[draft]`, `[agent]`, `[policy]`, `[browser]`, `[proactive]` and
`[plugins]` are accepted today but not read by anything yet. Each belongs to a daemon feature that
parses its own section when it starts; a feature whose section is invalid does not start (the error
is logged) and the rest of the daemon runs normally. The keys are not final; the plan for this wave:

| Section | Planned keys |
|---|---|
| `draft` | The model provider per app, apps excluded from Double Tap (password managers and banking apps by default), a context budget |
| `agent` | Settings of the agent runtime and computer use; not settled yet |
| `policy` | The safety policy for agent actions; rules are edited from the Safety page (`policy.set`) |
| `browser` | Settings of the browser extension bridge; not settled yet |
| `proactive` | `enabled`, the time of the morning brief, and where the card judge may run (its locality) |
| `plugins` | `trusted_keys` (trusted publisher keys) and a developer-mode switch that allows unsigned plugins |

When a feature lands, its keys get their own table on this page.

### `[proactive]`

Proactive cards and the morning brief (the Today page). Unknown keys are an error.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | `false`: no background refreshes and no brief; `cards.list` reports `enabled: false` |
| `brief_time` | `"HH:MM"` | `"08:00"` | Local time of the morning brief (`brief.ready`). A brief missed while the daemon was off is sent after the first refresh |
| `judge` | `"local"`, `"cloud"` or `"off"` | `"local"` | Where the optional card judge may run. `cloud` never receives sensitive candidates |
| `refresh_minutes` | integer ≥ 1 | `30` | Refresh cards this often (also after ingest and knowledge runs) |
| `debounce_secs` | integer | `60` | Wait this long after the last ingest or knowledge event before refreshing |
| `utc_offset` | `"+HH:MM"` | the machine's | The user's offset for "today" and the brief time; read once at start |

`[proactive.engine]` tunes the card generators and ranking (`nugget_proactive::ProactiveConfig`),
e.g. `min_score` (`0.45`, cards ranked below it are kept but not offered), `max_inbox`,
`user_addresses`, `followup_lead_days`, `awaiting_reply_days`, `renewal_horizon_days`.

## Example

```toml
[models]
download_parts = 4

[models.selected]
triage = "qwen3-4b-q4km"
embedding = "nomic-embed-text-v1.5-q8"

[runtime]
enabled = true
idle_stop_minutes = 30

[ingest]
allow_cloud_embeddings = false

[retrieve]
mmr = true

[retrieve.recency]
half_life_days = 30
weight = 0.2

[knowledge]
interval_minutes = 10

[knowledge.me]
name = "Ada Example"
emails = ["ada@example.com"]

[knowledge.llm]
cloud_sources = []

[vault]
path = "~/Nugget Vault"

[mcp]
http = true
port = 7319

# A LAN server, tried before the managed runtime.
[[providers]]
id = "workstation"
kind = "openai_compatible"
base_url = "http://192.168.1.20:8080/v1"
model = "qwen3-8b"
tiers = ["synthesis"]

# A cloud provider, tried last. The key comes from the daemon's environment.
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "<model name>"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["synthesis", "agentic"]
```
