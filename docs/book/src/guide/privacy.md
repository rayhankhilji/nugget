# Privacy

Nugget is built so you can answer "what left my machine?" from the app itself.

## The defaults

- **No Nugget servers and no telemetry.** Nothing is ever sent to the Nugget project.
- **No update checks until you choose** (Stable, Beta or Off, in setup or on the Status page). A
  check fetches a small signed JSON feed from GitHub with no identifiers.
- **Local models by default.** A cloud provider is used only if you add one.
- **Encrypted at rest.** The store is one SQLCipher (AES-256) database keyed from your OS keychain.
- **No network listener.** Only a local socket and, for [MCP](mcp.md), a token-protected listener
  on `127.0.0.1`.

## The egress ledger

Every request Nugget sends off this device is recorded with its time, host, feature, bytes sent and
received, and the items whose content was included. The **Privacy** page shows it, newest first
(IPC: [`egress.page`](../reference/ipc.md), or `egress.recent`). Requests to this device are not
recorded; requests to your local network are.

| Feature | Caused by |
|---|---|
| `chat`, `ask` | Chat or Ask with a non-local provider |
| `embed` | Embeddings to a non-local provider (only if you opted in, or a LAN server) |
| `triage` | Triage on a local-network provider (cloud is never used for triage) |
| `knowledge` | Fact extraction on a non-local provider (only sources you allowed) |
| `probe`, `prewarm` | Checking or warming up a provider (no data of yours) |
| `draft` | A [Double Tap](double-tap.md) or Today draft with a non-local provider |
| `agent` | [Agent](agent.md) model calls to a non-local provider |
| `proactive_judge` | The [Today](today.md) judge, if you allowed a cloud model |
| `sync:<kind>` | Connector syncs, including OAuth token refreshes (`sync:gmail`, `sync:imap`, `sync:slack`, …) |
| `sync:plugin:<id>`, `tool:plugin:<id>` | A [plugin](plugins.md)'s syncs and tool requests |
| `model_download`, `runtime_download` | Downloading a model or the `llama-server` runtime |
| `update_check`, `update_download` | Update checks and downloads (see [Install: Updates](install.md#updates)) |

**Limits:** the ledger covers Nugget's own traffic, not what an [MCP](mcp.md) app or your browser
does with your data. A crash mid-request can leave no row. Update rows come from the desktop app and
can be lost if it quits while the daemon is stopped. A "local" server that forwards to the cloud (such as Ollama's `-cloud` models) looks local. Mark
it `locality = "cloud"` in [`config.toml`](../reference/config.md#providers).

## Sensitive items

Every item is scanned locally for payment card numbers, US Social Security numbers, IBANs, and API
keys, tokens and private keys (only counts are kept, never the values). If the scan or the local
triage model flags it, or its status is unknown, the item is **sensitive**. Sensitive items:

- are never sent to a cloud model (triage, embeddings, knowledge, Double Tap, the Today judge);
- are left out when Ask uses a cloud model (the answer says how many were withheld);
- are hidden from MCP clients unless a client is allowed them;
- never reach the [Markdown vault](knowledge.md#sensitive-items-and-the-vault) once confirmed, and
  are hidden from the graph view unless you turn on **Show sensitive**.

## What leaves for each feature

| Feature | With a local model | With a cloud model |
|---|---|---|
| Ask and chat | Nothing | The question and selected non-sensitive passages |
| Knowledge extraction | Nothing | Non-sensitive item text, only from sources you listed |
| [Double Tap](double-tap.md) | Nothing (see [Read-ahead](double-tap.md#read-ahead)) | Field text, thread, subject, recipients, non-sensitive passages, voice examples |
| [Agent](agent.md) | Nothing | The goal, what it observes, and context from your data |
| [Today](today.md) | Nothing | Card evidence for the judge, only if allowed, never sensitive |
| [Plugins](plugins.md) | Requests to the hosts you approved | The same |

Your store, its key, your indexes and your connector credentials never leave the device (a
credential goes only to its own service).

## The audit log

The daemon also keeps an append-only, HMAC-chained **audit log** of changes: sources, sign-ins,
secrets set (name only), model installs, MCP calls and drafts (never content), and plugin installs.
`nuggetd status` reports whether the chain verified. The newest entry is anchored in your OS keychain
so truncation is detected (not with a key-file store). Agent and Today actions go to the separate
action log on the [Safety](safety.md) page.
