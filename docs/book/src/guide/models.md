# Local or bring-your-own-key models

Work is grouped into **tiers**, and each provider you configure says which tiers it serves:

| Tier | Used for | Default policy |
|---|---|---|
| `triage` | Per-item keep, junk or sensitive, plus a short summary, during indexing | **Local only**, never cloud |
| `light` | Cheap classification and routing calls | Any provider you assign |
| `synthesis` | Ask, chat, knowledge extraction, Double Tap drafting | Any provider; knowledge extraction is local-only unless you allow cloud per source |
| `agentic` | Tool-using agent loops and computer use | Any provider you assign |
| `embedding` | Vectors for semantic search | **Local or LAN only** unless you opt in |

## Option 1: fully local, managed by Nugget (recommended)

On the **Models** page, install the recommended models (all Apache-2.0). They come from a signed
manifest that pins each file's size and SHA-256, and are verified before install.

| Model id | Tiers | Download | Minimum RAM |
|---|---|---|---|
| `qwen3-4b-q4km` | triage, light, synthesis | 2.5 GB | 8 GB |
| `qwen3-8b-q4km` | synthesis, agentic | 5.0 GB | 16 GB |
| `nomic-embed-text-v1.5-q8` | embedding | 146 MB | 4 GB |
| `nomic-embed-text-v2-moe-q8` | embedding (multilingual; the pick for new indexes) | 512 MB | 4 GB |
| `bge-reranker-v2-m3-q4km` | rerank | 438 MB | 4 GB |

Nugget also downloads a verified llama.cpp `llama-server` for your hardware and runs it on
`127.0.0.1` as the providers `local-chat` and `local-embedding`. Servers stop after 30 idle minutes
and restart on demand ([`[runtime]`](../reference/config.md#runtime)); **Status** shows when a model
is warming up or needs a **Retry** (see
[Troubleshooting](troubleshooting.md#the-on-device-model-is-warming-up-stopped-or-wont-start)). These
downloads, logged in the egress ledger, are the only network traffic.

An index keeps the embedding model its vectors were built with, even if a newer manifest recommends
another, unless `[runtime] embedding_model` names one. An index built with an older model gets a
one-time offer on the **Models** page to **upgrade search to multilingual**: accepting downloads the
model, re-indexes in the background (paused on low battery and while you use the AI), and switches
over once every passage has its new vector; search keeps working on the old index until then. Cancel
or **Not now** keeps the old index, and the offer does not come back. Naming a different model for an existing index
makes search fall back to full-text, and new items get no vectors.

### Reranking search results

Optional. Install `bge-reranker-v2-m3-q4km`, then:

```toml
[runtime]
rerank_model = "bge-reranker-v2-m3-q4km"
```

Nugget runs a third server, `local-rerank`, and search reranks its top candidates with it. To use a
reranker you run yourself, see [`[retrieve.reranker]`](../reference/config.md#retrievereranker).

## Option 2: your own local server

Any OpenAI-compatible server works: Ollama, LM Studio, vLLM or your own `llama-server`. The default
`config.toml` points at Ollama:

```toml
[[providers]]
id = "local"
kind = "openai_compatible"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen3:4b"
tiers = ["triage", "light", "synthesis"]
```

Run `ollama pull qwen3:4b` and restart `nuggetd`. A server on `127.0.0.1` or `localhost` is
**on-device**; one on a private address is **local network** (recorded in the egress ledger, but
still allowed to triage and embed).

> **Forwarding servers** (such as Ollama's `-cloud` models) look local but send prompts to the cloud.
> Set `locality = "cloud"` on such a provider.

For a `llama-server` on another machine, set `llama_cpp = true` so it reuses its prompt cache (on by
default for on-device servers). See [`[[providers]]`](../reference/config.md#providers).

## Option 3: bring your own key (cloud)

Add a provider for Anthropic, OpenAI or any OpenAI-compatible cloud API. Keys are referenced as
`env:VAR` or `keychain:NAME` (service `dev.nugget.Nugget`, account `provider:NAME`); a plain key in
`config.toml` is rejected.

```toml
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "<model name from the provider>"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["synthesis", "agentic"]
```

Restart `nuggetd`. Then:

- Ask and chat may use it; every request is in the egress ledger with the items included.
- **Sensitive items are withheld** from cloud models; the answer says how many.
- Triage never uses it. Embeddings use it only if you opt in with `ingest.allow_cloud_embeddings`
  (indexing) and `retrieve.allow_cloud_embedding` (queries). Sensitive items are never embedded in the
  cloud.

## Routing and probing

For each tier, `nuggetd` tries on-device and local-network providers, then the managed runtime, then
cloud providers (each in file order), moving on after a network error or rate limit. Before a
provider is used beyond plain chat, a **probe** tests streaming, tool calls, JSON output, embeddings
and vision: run it from **Status** or with [`providers.probe`](../reference/ipc.md).
