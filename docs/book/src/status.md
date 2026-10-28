# What works today

This page is the honest summary. It reflects the `main` branch at the time of writing (milestones M0 and
M1 integrated, M2 crates built but not yet wired into the daemon, M3 to M6 as contracts or stubs). See
[`docs/PLAN.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/PLAN.md) for the full plan.

## Runs end to end

| Area | What you get |
|---|---|
| Daemon | `nuggetd` runs on macOS, Windows and Linux, in the foreground or as a per-user service. See [the CLI](reference/cli.md). |
| Encrypted store | SQLCipher database with full-text (FTS5) and vector (`sqlite-vec`) search, an egress ledger and a hash-chained audit log. |
| Models | Local models from a signed, pinned manifest, served by a supervised `llama-server` on loopback; or any OpenAI-compatible server (Ollama, LM Studio, vLLM); or Anthropic and OpenAI with your own key. |
| Sources | Folders, Obsidian vaults, Gmail, Google Calendar, IMAP and CalDAV, with live updates and deletes. |
| Search | Hybrid full-text plus vector search, one hit per item, with a link back to the original. |
| Ask | A question answered from retrieved passages, each cited as `[n]`. Sensitive items are withheld from cloud models. |
| Privacy | The Privacy page lists every request that left the device: host, feature, bytes and which items were included. |
| Desktop app | Chat, Search (with Ask), Sources, Models, Privacy and Status pages. |

## Built, not yet wired into the daemon

These crates exist and are tested on their own. The daemon does not run them yet, so the desktop pages
that use them show "not available".

| Area | Crate | Missing piece |
|---|---|---|
| Knowledge graph extraction | `nugget-knowledge`, `nugget-graph`, graph tables in `nugget-store` | The daemon does not run extraction or serve the `knowledge.*` IPC methods. |
| Markdown vault | `nugget-vault` | Not rendered or watched by the daemon yet. |
| MCP server | `nugget-mcp` | No `nuggetd mcp` subcommand, no HTTP listener and no `mcp.clients.*` methods in the daemon yet. |
| Knowledge page, graph view, Connected apps page | `apps/desktop` | Built against the IPC contract; waiting on the daemon. |

## Contracts and stubs only

Double Tap drafting (M3: `nugget-a11y`, `nugget-hotkey`, `nugget-draft`, `nugget-voice`), computer use
and the agent (M4: `nugget-agent`, `nugget-policy`, `nugget-browser`), proactive cards (M5:
`nugget-proactive`), WASM plugins (M6: `nugget-plugin`), and the Microsoft 365 and SaaS connectors
(`nugget-connector-microsoft`, `nugget-connector-saas`). The `draft.*` and `voice.status` IPC methods are
defined in the protocol but the daemon answers them with "method not found".

## Not started

Installers and signed releases, auto-update, the end-to-end-encrypted relay, internationalisation, and
the `draft`, `cua` and `knowledge` benchmark suites.
