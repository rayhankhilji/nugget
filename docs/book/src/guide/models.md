# Local or bring-your-own-key models

Nugget never routes work to a specific model by name. Work is grouped into **tiers**, and each
provider you configure says which tiers it serves:

| Tier | Used for | Default policy |
|---|---|---|
| `triage` | Per-item keep, junk or sensitive, plus a short summary, during indexing | **Local only.** Cloud providers are never used for triage. |
| `light` | Cheap classification and routing calls | Any provider you assign |
| `synthesis` | Ask, chat, knowledge extraction (when wired in), drafting (planned) | Any provider you assign |
| `agentic` | Tool-using agent loops and computer use (planned, M4) | Any provider you assign |
| `embedding` | Vectors for semantic search, at indexing and query time | **Local or LAN only** unless you opt in |

There are three ways to serve a tier.

## Option 1: fully local, managed by Nugget (recommended)

On the **Models** page, install the recommended models. They come from a signed manifest that pins
each file's exact upstream revision, size and SHA-256. Downloads are checked before anything is
installed, and the install refuses to start if your disk is too full. The current manifest has:

| Model id | Tiers | Download | Minimum RAM | Licence |
|---|---|---|---|---|
| `qwen3-4b-q4km` | triage, light, synthesis | 2.5 GB | 8 GB | Apache-2.0 |
| `qwen3-8b-q4km` | synthesis, agentic | 5.0 GB | 16 GB | Apache-2.0 |
| `nomic-embed-text-v1.5-q8` | embedding | 146 MB | 4 GB | Apache-2.0 |

When a selected model is installed, `nuggetd` downloads a pinned, verified build of llama.cpp's
`llama-server` for your OS and hardware (Metal, Vulkan or CPU), and runs one server for chat and one for
embeddings on `127.0.0.1`. They appear as the providers `local-chat` (triage, light, synthesis) and
`local-embedding` (embedding). Each server requires a random API key that changes on every restart.
Servers stop after 30 idle minutes and start again on the next request
([`[runtime]`](../reference/config.md#runtime)).

The model and runtime downloads are the only network traffic in this setup, and both appear in the
egress ledger.

## Option 2: your own local server

Any OpenAI-compatible server works: Ollama, LM Studio, vLLM or a `llama-server` you run yourself. The
default `config.toml` already points at Ollama:

```toml
[[providers]]
id = "local"
kind = "openai_compatible"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen3:4b"
tiers = ["triage", "light", "synthesis"]
```

Pull the model (`ollama pull qwen3:4b`) and restart `nuggetd`. A server on `127.0.0.1` or `localhost`
counts as **on-device**. One on a private address (`192.168.x.x`, `10.x.x.x` and so on) counts as
**local network**: its traffic is recorded in the egress ledger, but it may still triage and embed.

> **Watch out for forwarding servers.** Some local servers forward to the cloud. Ollama's `-cloud`
> models, for example, are served from localhost but send your prompt to ollama.com. Nugget cannot tell
> from the address. Mark such a provider with `locality = "cloud"` so it is treated, and recorded, as
> cloud.

## Option 3: bring your own key (cloud)

Add a provider for Anthropic, OpenAI or any OpenAI-compatible cloud API (OpenRouter, for example). Keys
are never written in the config file: reference them as `env:VAR` (an environment variable) or
`keychain:NAME` (an OS keychain entry under service `dev.nugget.Nugget`, account `provider:NAME`).
A plain key in `config.toml` is rejected at load time.

```toml
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-opus-5-5"
api_key = "env:ANTHROPIC_API_KEY"
tiers = ["synthesis", "agentic"]
```

Then restart `nuggetd`. What changes when a cloud provider is configured:

- **Ask** and **chat** may use it, and every request is recorded in the egress ledger with the items
  whose content was included.
- **Sensitive items are withheld** from cloud models in Ask. The answer tells you how many were left
  out.
- **Triage never** uses it. **Embeddings** use it only if you opt in:
  `ingest.allow_cloud_embeddings` sends non-sensitive items' chunks to a cloud embedder at indexing
  time, and `retrieve.allow_cloud_embedding` sends your search queries (and, for diversity ranking,
  passages stored without a vector). Sensitive items are never embedded in the cloud.

## Routing and probing

For each tier, `nuggetd` tries providers in this order: the on-device and local-network providers from
your config (in file order), then the managed runtime, then cloud providers (in file order). If one
fails with a network error or a rate limit, the next is tried.

A provider has to **prove** what it can do before it is used for anything beyond plain chat. A probe
sends live test requests: streaming, tool calls, JSON-schema output, embeddings, and vision (it renders
a random four-digit code into an image and checks the model reads it back). Run it from the **Status**
page or with the [`providers.probe`](../reference/ipc.md) IPC method. Results are saved in `probes.json`
and reused after a restart.

See [`config.toml`](../reference/config.md#providers) for every provider field.
