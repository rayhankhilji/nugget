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

If Ollama (`127.0.0.1:11434`) or LM Studio (`127.0.0.1:1234`) is running, the AI step of setup and
the **Models** page list it under **Found on this computer** with its installed models. Pick a model
and choose **Use Ollama** (or **Use LM Studio**): Nugget checks the server, adds it without a key and
lets it triage too, since it runs on this computer. Models that forward prompts to a cloud service
(Ollama's `-cloud` models) are not offered there; added by hand, they are saved with
`locality = "cloud"` and never triage.

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

In setup or on the **Models** page (**Add a provider**), pick a preset, paste the key and choose
**Check and save key**. Nugget makes one cheap call (it lists the provider's models), says whether
the key works, and offers the listed models in the model fields. The key goes to your OS keychain;
`config.toml` only gets a `keychain:NAME` reference. The check is in the egress ledger (feature
`ai_setup`).

Each preset fills in the address, a **Model** (chat, Ask, Double Tap drafting and the agent: tiers
`synthesis` and `agentic`) and a **Model for quick tasks** (tier `light`, saved as a second provider
`NAME-light` that shares the key). Clear the second field to use one model for everything. Triage
never uses a cloud preset, and the privacy rules below apply to all of them.

| Preset | Kind | Address | Model | Quick tasks | Key page |
|---|---|---|---|---|---|
| Anthropic | `anthropic` | `https://api.anthropic.com` | `claude-opus-5-5` | `claude-haiku-5-5` | [platform.claude.com](https://platform.claude.com/settings/keys) |
| OpenAI | `openai_responses` | `https://api.openai.com/v1` | `gpt-6-astra` | `gpt-6-luna` | [platform.openai.com](https://platform.openai.com/api-keys) |
| Google Gemini | `openai_compatible` | `https://generativelanguage.googleapis.com/v1beta/openai` | `gemini-4-argon` | `gemini-3.5-flash-lite` | [aistudio.google.com](https://aistudio.google.com/apikey) |
| OpenRouter | `openai_compatible` | `https://openrouter.ai/api/v1` | `anthropic/claude-sonnet-5.5` | `google/gemini-3.8-flash` | [openrouter.ai](https://openrouter.ai/keys) |
| Groq | `openai_compatible` | `https://api.groq.com/openai/v1` | `openai/gpt-oss-120b` | `llama-3.1-8b-instant` | [console.groq.com](https://console.groq.com/keys) |
| Mistral | `openai_compatible` | `https://api.mistral.ai/v1` | `mistral-medium-latest` | `mistral-small-latest` | [console.mistral.ai](https://console.mistral.ai/api-keys) |
| DeepSeek | `openai_compatible` | `https://api.deepseek.com` | `deepseek-v4-pro` | `deepseek-flash` | [platform.deepseek.com](https://platform.deepseek.com/api_keys) |
| Together AI | `openai_compatible` | `https://api.together.ai/v1` | `deepseek-ai/DeepSeek-V4-Pro-0813` | `Qwen/Qwen3.8-Flash` | [api.together.ai](https://api.together.ai/settings/projects/~current/api-keys) |

Per preset:

- **Anthropic**: the Messages API. The model list is at `/v1/models`.
- **OpenAI**: the Responses API, with responses not stored at OpenAI (`store = false`).
- **Google Gemini**: Google's OpenAI-compatible endpoint (in beta at Google). A wrong key is answered
  with HTTP 400, which Nugget reports as a rejected key; listed ids lose their `models/` prefix.
- **OpenRouter**: its model list answers without a key, so Nugget checks the key at
  `/api/v1/key` first. Model ids are `author/model`.
- **Groq**, **Mistral**, **DeepSeek**: plain OpenAI-compatible chat completions. DeepSeek's address
  has no `/v1`.
- **Together AI**: OpenAI-compatible; its model list may be a bare array, which Nugget accepts.
- **OpenAI-compatible (other)**: enter the address yourself (it usually ends in `/v1`). A server on
  `localhost` or `127.0.0.1` needs no key.

Model names change: if a check says the model is not in the provider's list, pick one from the
suggestions. Which feature uses which provider stays with the tier router (see
[Routing and probing](#routing-and-probing)): on-device providers and the managed runtime come first
in every tier, so with a local model installed, a cloud preset is the fallback, or the provider you
pick in Chat.

### By hand

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
