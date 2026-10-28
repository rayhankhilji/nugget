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
- **No listener on the network.** The daemon listens on a local socket only.

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
| `knowledge` | LLM fact extraction on a non-local provider (once the knowledge graph is wired in) |
| `probe` | Capability probes of a non-local provider |
| `sync:gmail`, `sync:gcal`, `sync:imap`, `sync:caldav` | Connector syncs, including OAuth token refreshes |
| `model_download`, `runtime_download` | Downloading a model or the `llama-server` runtime |

Requests to a server on **this device** (loopback) are not recorded, because nothing leaves the
machine. Requests to your **local network** are recorded.

### Limits of the ledger

- It records what Nugget's own network layer sends. Code that bypassed it would be a bug; plugins
  (planned) will have no direct network access for this reason.
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

- are never sent to a cloud model for triage, embedding or (once wired in) knowledge extraction;
- are left out of the context when Ask uses a cloud model (the answer says how many were withheld);
- are hidden from MCP clients unless a client is explicitly allowed to see them.

## What never leaves your device

- Your store, its encryption key, and every index.
- Your connector credentials (passwords, OAuth tokens, OAuth client secrets): they go only to the
  service they belong to, for example a Google token only to Google.
- Anything, with the default configuration, beyond connector syncs and model downloads.
- Sensitive items, to any cloud model.

What **does** leave, when you configure it: the question and the selected passages, to the cloud
provider you chose for Ask or chat; and your sources' own traffic to their servers.

## The audit log

Separately from the egress ledger, the daemon keeps an append-only **audit log** of changes: sources
added, updated and removed, sign-ins started and finished, secrets set (the account name only, never
the value), model installs. Each entry includes a SHA-256 hash of the previous one, so editing or
deleting old entries is detectable; `nuggetd status` reports whether the chain verified at startup.
The hash is not keyed yet, so someone who has your store key could rewrite the whole chain; keying it
is planned.
