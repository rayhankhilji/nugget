# What works today

This page is the honest summary of the integration branch after waves W and X: every milestone,
M0 to M6, runs in the daemon and the desktop app, and CI is green on macOS, Windows and Linux. What
is still missing is evidence (real desktop runs on macOS and Windows, numbers with real models) and
a signed release. See [`docs/PLAN.md`](https://github.com/rayhankhilji/nugget/blob/main/docs/PLAN.md)
for the plan and its v1 checklist.

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
| Desktop app | Today, Chat (answers from your sources with citations), Search (with Ask), Knowledge (with the graph view), Sources, Connected AI apps, Plugins, Agent, Safety, Models, Double Tap, Privacy and Status pages, with a first-run onboarding flow, in nine languages (some catalogs are partial). From an empty install to a cited search result takes a few seconds on the bundled sample folder (measured by the desktop e2e). |
| Double Tap | Press a shortcut in any app to get a draft in your voice, written from what Nugget knows, inserted where you type. See [Double Tap](guide/double-tap.md). |
| Agent and computer use | An agent that uses your desktop apps through the accessibility APIs and your browser through the extension, behind the safety policy: risk classes, confirmations, budgets, a kill switch (Escape ×3) and a hash-chained action log. See [The agent](guide/agent.md) and [Safety](guide/safety.md). |
| Proactive cards and the morning brief | The Today page: cards for things that need you, each with why it is shown and a dry-run of what it would do; nothing runs until you approve it. See [Today](guide/today.md). |
| Plugins | WebAssembly plugins with declared, user-approved permissions. See [Plugins](guide/plugins.md). |
| More connectors | Microsoft 365 (Outlook mail and calendar, OneDrive), Slack, Notion, GitHub and Linear. See [Microsoft 365](guide/sources-microsoft.md) and [Slack, Notion, GitHub, Linear](guide/sources-saas.md). |

## Not yet verified

| Area | State |
|---|---|
| macOS and Windows desktops | CI builds and tests every crate on all three OSes, but Double Tap, computer use and the installers have only been exercised hands-on on Linux. Real-desktop runs on macOS and Windows are in progress. |
| Real models | Published benchmark numbers come from scripted or fake models, labelled as such. Runs with real local models are in progress. |
| Live accounts | The Microsoft 365 and SaaS connectors are tested against mock servers, not live accounts. |

## Built, not released

| Area | State |
|---|---|
| Installers and signed updates | The release workflow's dry run builds unsigned installers for macOS (Apple Silicon and Intel), Windows and Linux (x64 and arm64), and the Linux AppImage is launch-tested in CI. Signing and notarisation need release keys, and no release has been cut. The in-app updater (user-chosen channel, opt-in) is in place. Build from source: see [Install](guide/install.md). |
| Benchmarks | Every suite runs nightly and publishes to the [benchmark dashboard](benchmarks/dashboard.md); the README table is generated from the latest results. `draft` and `cua` use deterministic fake or scripted models unless you point them at a real provider. |

## Not started

The end-to-end-encrypted relay (optional in the plan), and an OSWorld-style subset for the `cua`
benchmark.
