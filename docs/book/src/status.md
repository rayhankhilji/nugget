# What works today

This page is the honest summary. It reflects the integration branch during wave W: milestones M0, M1
and M2 are integrated and running in the daemon; M3 to M6 are built as tested crates and are being
wired into the daemon and the desktop app in wave W. See
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
| Knowledge graph | People, organisations, projects, commitments and other entities with cited facts, extracted after indexing (deterministic extractors always; an LLM extractor on local models, or cloud for sources you allow). See [The knowledge graph and vault](guide/knowledge.md). |
| Vault | The graph rendered as Markdown pages (`~/Nugget Vault` by default), with your edits merged back. |
| MCP server | Read-only tools for Claude Desktop, Claude Code, Cursor and other apps, per-app tokens and scopes, over `nuggetd mcp` (stdio) or loopback HTTP; `nuggetd mcp install` sets an app up. See [MCP](guide/mcp.md). |
| Privacy | The Privacy page lists every request that left the device: host, feature, bytes and which items were included. |
| Desktop app | Chat, Search (with Ask), Sources, Models, Knowledge (with the graph view), Connected AI apps, Privacy and Status pages, with a first-run onboarding flow, in nine languages (some catalogs are partial). |

Known M2 follow-ups, being done in wave W: the IPC contract does not yet report the sticky `rejected`
fact status (it shows as `retracted`), and the `knowledge` benchmark still misses some targets
(evidence quotes and duplicate entities in a few fixture cases).

## Being wired in during wave W

These crates exist and are tested on their own. Wave W is connecting them to the daemon and the
desktop app on separate branches; none of them runs in the app on the integration branch yet, and the
daemon answers their IPC methods with "method not found".

| Area | Milestone | Crates | Guide |
|---|---|---|---|
| Double Tap: drafting in your voice in any text field | M3 | `nugget-hotkey`, `nugget-a11y`, `nugget-draft`, `nugget-voice` | [Double Tap](guide/double-tap.md) |
| The agent, with the browser extension | M4 | `nugget-agent`, `nugget-browser`, `extensions/browser` | [The agent](guide/agent.md) |
| Computer use on desktop apps through the accessibility APIs | M4 | `nugget-a11y` (a computer-use tools crate is being written) | [The agent](guide/agent.md) |
| Safety policy, kill switch (Escape ×3), confirmations, action log | M4 | `nugget-policy` | [Safety](guide/safety.md) |
| Proactive cards and the morning brief | M5 | `nugget-proactive` | [Today](guide/today.md) |
| WebAssembly plugins | M6 | `nugget-plugin` and its SDK | [Plugins](guide/plugins.md) |
| Microsoft 365 (Outlook mail and calendar, OneDrive), Slack, Notion, GitHub and Linear connectors | M6 | `nugget-connector-microsoft`, `nugget-connector-saas` | [Microsoft 365](guide/sources-microsoft.md), [Slack, Notion, GitHub, Linear](guide/sources-saas.md) |

## Built, not released

| Area | State |
|---|---|
| Installers and signed updates | The release pipeline, bundle configuration and in-app updater (user-chosen channel, opt-in) are in the repository, but no release has been cut. Build from source: see [Install](guide/install.md). |
| Benchmarks | The `memory`, `perf`, `knowledge`, `draft`, `cua` and `proactive` suites exist. `draft` uses a deterministic fake model unless you point it at a real provider; `cua` runs simulated apps with reference agents until the real agent is plugged in; `proactive` runs a reference engine until the real crate is wired in. Published results so far: see [Benchmarks](architecture/benchmarks.md). |

## Not started

The end-to-end-encrypted relay (optional in the plan), and an OSWorld-style subset for the `cua`
benchmark.
