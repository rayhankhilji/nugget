# Privacy

Nugget is built so that you can answer, from the app itself, the question "what left my machine?".

## The defaults

- **No Nugget servers.** Nugget runs no first-party service in the data path. Nothing is sent to the
  Nugget project, ever.
- **No telemetry.** There is none at all today. If it is ever added, it will be opt-in.
- **Local models by default.** The default config uses only a model server on `127.0.0.1`. A cloud
  provider is used only if you add one.
- **Encrypted at rest.** The store (items, chunks, the search indexes, the graph, the ledgers) is one
  SQLCipher database (AES-256), keyed by a random key in your OS keychain.
- **No listener on the network.** The daemon listens on a local socket, plus, for
  [MCP](mcp.md), an HTTP listener on `127.0.0.1` that needs a per-app token. Nothing is reachable
  from another machine.

## The egress ledger

Every request Nugget's own network layer sends to a destination that is not on this device is recorded
**before you look for it**, with:

| Field | Example |
|---|---|
| When | `2026-10-28T09:14:03Z` |
| Host | `api.anthropic.com` |
| Feature | `ask` |
| Bytes sent and received | 18 204 / 2 911 |
| Items included | the items whose content (or derived text) was in the request |

The **Privacy** page shows the ledger, newest first. Over IPC it is
[`egress.recent`](../reference/ipc.md).

Features you will see:

| Feature | Caused by |
|---|---|
| `chat` | A chat in the Chat page to a non-local provider |
| `ask` | Ask with context to a non-local provider |
| `embed` | Embeddings sent to a non-local embedding provider (only if you opted in, or a LAN server) |
| `triage` | Triage on a local-network provider (cloud is never used for triage) |
| `knowledge` | LLM fact extraction on a non-local provider (only for sources you allowed) |
| `probe` | Capability probes of a non-local provider |
| `sync:gmail`, `sync:gcal`, `sync:gdrive`, `sync:imap`, `sync:caldav` | Connector syncs, including OAuth token refreshes |
| `model_download`, `runtime_download` | Downloading a model or the `llama-server` runtime |

And for the newer features:

| Feature | Caused by |
|---|---|
| `draft` | A [Double Tap](double-tap.md) draft, or a draft reply on a Today card, made by a non-local provider |
| `agent` | Model calls of the [agent](agent.md) to a non-local provider |
| `proactive_judge` | The optional judge for [Today cards](today.md), when you allowed a cloud model |
| `sync:plugin:<id>`, `tool:plugin:<id>` | A [plugin](plugins.md) connector's syncs and a plugin tool's requests |
| `sync:outlook`, `sync:outlook-calendar`, `sync:onedrive`, `sync:slack`, `sync:notion`, `sync:github`, `sync:linear` | The Microsoft 365 and SaaS connectors |

Requests to a server on **this device** (loopback) are not recorded, because nothing leaves the
machine. Requests to your **local network** are recorded.

### Limits of the ledger

- It records what Nugget's own network layer sends. Code that bypassed it would be a bug. Plugins
  have no network access of their own for this reason: every request goes through the host, which
  records it.
- It does not record what other programs do with what Nugget gives them. An app connected over
  [MCP](mcp.md) may send the passages it reads to its own cloud model; that is the app's traffic.
  Likewise, pages the [agent](agent.md) opens in your browser are your browser's traffic.
- A row is written when an exchange ends, so a crash in the middle of a request can leave no row. A
  two-phase record (written before the first byte) is planned.
- A "local" server that forwards to the cloud (for example Ollama's `-cloud` models) looks local.
  Mark it `locality = "cloud"` in [`config.toml`](../reference/config.md#providers).

## Sensitive items

During indexing, every item is scanned, deterministically and locally, for:

- payment card numbers (Luhn-checked),
- US Social Security numbers,
- IBANs (checksum-verified),
- API keys, tokens and PEM private keys.

Only counts are kept; the matched values are never stored or logged. If the scan finds anything, or a
local triage model flags the item, the item is marked **sensitive**. Items whose status is unknown are
treated as sensitive. Sensitive items:

- are never sent to a cloud model for triage, embedding or knowledge extraction, nor for Double Tap
  drafts or the Today judge;
- are left out of the context when Ask uses a cloud model (the answer says how many were withheld);
- are hidden from MCP clients unless a client is explicitly allowed to see them.

## What leaves for each feature

| Feature | With a local model | With a cloud model |
|---|---|---|
| Ask and chat | Nothing | The question and the selected, non-sensitive passages |
| Knowledge extraction | Nothing | Text of non-sensitive items, only from sources you listed |
| [Double Tap](double-tap.md) | Nothing | The field text, the thread read around it, subject and recipients, non-sensitive passages and your voice examples. Screenshots only if you allow cloud screenshots |
| [Agent](agent.md) | Nothing | The goal, observations of the apps and pages it works in, and any context from your data |
| [Today](today.md) | Nothing; cards are generated and ranked on your device | Card evidence for the judge, only if you allowed a cloud judge, never sensitive evidence |
| [Plugins](plugins.md) | Requests to the hosts you approved for the plugin, whichever model you use | The same |

"Local" includes a server on your local network; such requests stay off the internet but are still
recorded in the ledger. Every cloud request is recorded with the feature named above.

Double Tap never reads password or secure fields, nor anything in password managers, banking apps or
system settings. The agent's [safety policy](safety.md) denies acting in those by default.

## What never leaves your device

- Your store, its encryption key, and every index.
- Your connector credentials (passwords, OAuth tokens, OAuth client secrets): they go only to the
  service they belong to, for example a Google token only to Google. A plugin's secrets go only to
  the hosts approved for that plugin.
- Anything, with the default configuration, beyond connector syncs and model downloads.
- Sensitive items, to any cloud model.

## The audit log

Separately from the egress ledger, the daemon keeps an append-only **audit log** of changes: sources
added, updated and removed, sign-ins started and finished, secrets set (the account name only, never
the value), model installs, every MCP call (arguments summarised, never content), every draft (without its
content) and plugin installs and approvals. Each new entry is chained to the previous one with an HMAC-SHA256 keyed from
your store key, so editing or deleting old entries is detectable; `nuggetd status` reports whether
the chain verified at startup. The newest entry is also anchored in your OS keychain, so cutting off
the newest entries is detected too. With a key-file store (no keychain), or if that keychain entry is
deleted, there is no anchor and such truncation is not detected.

Actions taken on your behalf by the [agent](agent.md) and by [Today cards](today.md) also go into a
separate hash-chained **action log**, exportable from the [Safety](safety.md) page.
