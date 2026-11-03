# Local or bring-your-own-key models

Work is grouped into **tiers**, and each provider you configure says which tiers it serves:

| Tier | Used for | Default policy |
|---|---|---|
| `triage` | Per-item keep, junk or sensitive, plus a short summary, during indexing | **Local only**, never cloud |
| `light` | Cheap classification and routing calls | Any provider you assign |
| `synthesis` | Ask, chat, knowledge extraction, Double Tap drafting | Any provider; knowledge extraction is local-only unless you allow cloud per source |
| `agentic` | Tool-using agent loops and computer use | Any provider you assign |
| `embedding` | Vectors for semantic search | **Local or LAN only** unless you opt in |

## Option 1: fully local, managed by Heed (recommended)

On the **Models** page, install the recommended models (all Apache-2.0). They come from a signed
manifest that pins each file's size and SHA-256, and are verified before install.

| Model id | Tiers | Download | Minimum RAM |
|---|---|---|---|
| `qwen3-4b-q4km` | triage, light, synthesis | 2.5 GB | 8 GB |
| `qwen3-8b-q4km` | synthesis, agentic | 5.0 GB | 16 GB |
| `nomic-embed-text-v1.5-q8` | embedding | 146 MB | 4 GB |
| `nomic-embed-text-v2-moe-q8` | embedding (multilingual; the pick for new indexes) | 512 MB | 4 GB |
| `bge-reranker-v2-m3-q4km` | rerank | 438 MB | 4 GB |

Heed also downloads a verified llama.cpp `llama-server` for your hardware and runs it on
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

Heed runs a third server, `local-rerank`, and search reranks its top candidates with it. To use a
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
`env:VAR` or `keychain:NAME` (service `app.heed.Heed`, account `provider:NAME`); a plain key in
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

In the desktop app, **AI → Add a provider** offers presets for **Anthropic** (paste a key from
console.anthropic.com), **OpenAI**, **OpenRouter** (`https://openrouter.ai/api/v1`), **Ollama**
(`http://127.0.0.1:11434/v1`, no key) and **any OpenAI-compatible endpoint** (base URL, optional
key). Each key is checked by listing the provider's models before it is saved to the OS keychain.
There is no "Sign in with Claude": Anthropic does not allow other apps to use Claude.ai logins, so
Claude is used with an API key.

## Option 4: sign in with ChatGPT (local mode only)

If you run Heed yourself, you can sign in with your ChatGPT account and let your ChatGPT plan pay
for the light and smart tiers, through OpenAI's programme for open-source and locally run apps.

- **How it works.** Heed opens OpenAI's sign-in page in your browser, which comes back to Heed on
  `127.0.0.1`. Heed never sees your password.
  - The first sign-in registers this installation with OpenAI (an opaque per-install id, kept in
    `chatgpt-host.json` in the data directory).
  - The tokens are kept in the OS keychain only, and are refreshed in the background.
  - Requests go from this computer straight to `api.openai.com`, with OpenAI storage off
    (`store: false`).
- **Plan usage must be granted.** If the sign-in comes back without ChatGPT plan usage
  (`chatgpt.tokens.use.direct`), nothing is sent. The app offers to try again or to use an API key
  instead.
- **Limits.** When your plan's usage limit is reached, ChatGPT requests pause and the app shows
  **Manage usage** (chatgpt.com/settings/usage).
  - Heed never moves the request to another, possibly paid, provider on its own.
  - Choose **Resume** after checking your usage, or pick another AI.
- **Scope.** Triage of local items and embeddings stay on this computer; they never use the plan.
- **Not with Heed Cloud.** Sign in with ChatGPT is for local mode only; the hosted Heed Cloud never
  uses ChatGPT plan tokens.
- **Signing out** revokes the tokens and deletes them from the keychain. The installation's
  registration is kept, so the next sign-in reuses it.

The daemon methods are `ai.chatgpt.status`, `ai.chatgpt.sign_in`, `ai.chatgpt.cancel`,
`ai.chatgpt.resume` and `ai.chatgpt.sign_out`.

## Routing and probing

For each tier, `nuggetd` tries on-device and local-network providers, then the managed runtime, then
cloud providers (each in file order), moving on after a network error or rate limit. Before a
provider is used beyond plain chat, a **probe** tests streaming, tool calls, JSON output, embeddings
and vision: run it from **Status** or with [`providers.probe`](../reference/ipc.md).
