# config.toml

`nuggetd` reads `config.toml` from the config directory (see [Data directory](data-dir.md)) once, at
startup. Restart the daemon after editing it. If the file does not exist, the daemon writes a commented,
local-first default there (readable only by you) and says so.

**Never put secrets in this file.** API keys and credential-bearing headers must be references:
`env:VAR` (an environment variable of the daemon) or `keychain:NAME` (an OS keychain entry, service
`dev.nugget.Nugget`, account `provider:NAME`). A plaintext key is rejected.

**Validation.** Unknown sections and unknown keys in `[models]`, `[models.selected]`, `[runtime]` and
`[[providers]]` are errors, as are out-of-range values, duplicate provider ids and malformed model ids.
Unknown keys inside `[ingest]` and `[retrieve]` are currently **ignored**, not rejected, so check their
spelling against this page.

<!-- check-refs: config.sections -->
| Section | What it configures |
|---|---|
| `models` | Model downloads, and the local model chosen per tier |
| `runtime` | The on-device `llama-server` runtime that `nuggetd` manages |
| `ingest` | Indexing: extraction, chunking, triage, embeddings, scheduling |
| `retrieve` | Search and context packing |
| `providers` | Model providers, as an array of tables (`[[providers]]`) |

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

### Locality

| Address | Locality | Egress ledger |
|---|---|---|
| Loopback (`127.0.0.1`, `::1`, `localhost`) | `on_device` | Not recorded: nothing leaves the machine |
| Private ranges (`10/8`, `172.16/12`, `192.168/16`, link-local, ...) | `local_network` | Recorded |
| Anything else | `cloud` | Recorded |

Host names are resolved once, when the provider is built, and the result is pinned.

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
model = "claude-opus-5-5"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["synthesis", "agentic"]
```
