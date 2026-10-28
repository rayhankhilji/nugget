# Crate map

Nugget is one Cargo workspace. Shared contracts live in `nugget-core`, and every other crate builds
against them, which is what lets several people (and agents) build crates in parallel.

Every crate in the workspace is implemented and tested. What differs is whether the daemon or the app
runs it yet:

- **in use**: `nuggetd` or the desktop app runs it today (M0 to M2).
- **being wired**: built and tested, and being wired into the daemon and app in wave W (M3 to M6).
  It is not part of the product yet.

```text
                          apps/desktop (Tauri 2: React + TypeScript)
                                     │ local IPC (nugget-ipc)
┌────────────────────────────────────── nuggetd ───────────────────────────────────────┐
│ connectors ─▶ nugget-ingest ─▶ nugget-store ─▶ nugget-retrieve ─▶ IPC handlers       │
│ (files, google, imap,  │ extract, PII,  │ SQLCipher, FTS5,   │ hybrid, MMR,          │
│  caldav; nugget-oauth) │ triage, chunk, │ sqlite-vec, ledger,│ cited packing         │
│                        │ embed          │ audit, graph       │                       │
│ nugget-knowledge ─▶ graph ─▶ nugget-vault     nugget-mcp ◀── MCP apps (stdio, HTTP)  │
│ nugget-infer (providers, router) ◀── nugget-runtime (llama-server) ◀── nugget-models │
│                                                                                      │
│ features/ (wave W): draft, agent, policy, browser, proactive, plugins                │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

Subsystems that arrive after M2 plug into the daemon through `crates/nuggetd/src/features/`: one
module per feature, owning its IPC methods, background tasks and `config.toml` section. The seam is in
place; no feature is registered in it yet.

<!-- check-refs: crates -->
| Crate | Milestone | State | Responsibility |
|---|---|---|---|
| `nugget-core` | M0 | in use | Shared contracts: ids, errors, capability tiers and locality, egress events, paths, keychain naming, the IPC protocol types |
| `nugget-store` | M0 | in use | The SQLCipher store: items, chunks, FTS5 and `sqlite-vec` search with rank fusion, cursors, the item change feed, egress ledger, hash-chained audit log, knowledge-graph tables (`SqliteGraph`) |
| `nugget-infer` | M0 | in use | One `Provider` trait over Anthropic Messages, OpenAI Responses and OpenAI-compatible servers; locality classification; capability probe; tier router with fallback; prompt caching and prewarm |
| `nugget-models` | M0 | in use | Signed model and runtime manifest, verified resumable downloads, content-addressed storage with rollback, hardware detection and recommendation |
| `nugget-ipc` | M0 | in use | JSON-RPC 2.0 over Unix sockets and Windows named pipes: server, client, limits, the single-daemon lock |
| `nuggetd` | M0 | in use | The daemon: config, startup and shutdown, IPC handlers, sources wiring, local runtime slots, knowledge, vault and MCP wiring, `nuggetd mcp`, service install, the `features/` seam |
| `nugget-connector` | M1 | in use | The connector API: `Connector`, `Change`, `ChangeSink`, `ConnectorContext`, `ConnectorFactory` registry, metered HTTP |
| `nugget-extract` | M1 | in use | Bytes to text and structure (Markdown, HTML, PDF, DOCX, XLSX, CSV, MIME mail, iCalendar, code, OCR), and the structure-aware chunker |
| `nugget-ingest` | M1 | in use | The ingest runtime: `sources.toml`, a supervisor per source, the pipeline (extract, PII scan, triage, chunk, embed, store), cursor-after-persistence, power policy, freshness metrics, re-embedding |
| `nugget-connector-files` | M1 | in use | `files` and `obsidian`: scan, ignore files, native watcher with debounce, reconcile |
| `nugget-oauth` | M1 | in use | OAuth 2.0 authorization code with PKCE and loopback redirect, token storage and refresh |
| `nugget-connector-google` | M1 | in use | `gmail` (history deltas) and `gcal` (sync tokens) |
| `nugget-connector-imap` | M1 | in use | `imap`: TLS only, UIDVALIDITY, CONDSTORE and QRESYNC, IDLE |
| `nugget-connector-caldav` | M1 | in use | `caldav`: discovery, RFC 6578 sync, ETag fallback |
| `nugget-retrieve` | M1 | in use | Query embedding, over-fetched hybrid search, per-item grouping, MMR, token-budgeted packing with `[n]` citations, the sensitive filter, an eval harness |
| `nugget-runtime` | M1 | in use | Supervised `llama-server` processes on loopback: install, spawn, health, restart, idle stop, per-start API keys |
| `nugget-graph` | M2 | in use | Graph types, the `GraphStore` trait, the in-memory reference implementation and conformance tests |
| `nugget-knowledge` | M2 | in use | Extraction (deterministic and LLM), entity resolution, claim lifecycle, run by the daemon after ingest |
| `nugget-vault` | M2 | in use | Markdown vault rendering, three-way merge, edits to graph changes, validators, watcher |
| `nugget-mcp` | M2 | in use | The local MCP server: read-only tools, resources, per-client tokens and scopes, rate limits, audit, stdio and loopback HTTP |
| `nugget-a11y` | M3 | in use | Read the focused field and its thread; insert with read-back verification (macOS AX, Windows UI Automation, Linux AT-SPI); actions for computer use |
| `nugget-hotkey` | M3 | in use | Global trigger: a double tap of a modifier, or a chord, on macOS, Windows and Linux; observes only the configured trigger |
| `nugget-draft` | M3 | in use | Rule-based field classification, the secure-field and excluded-app gate, the drafting engine with budgeted, cited context |
| `nugget-voice` | M3 | in use | The user's writing-style profile and exemplars per channel and recipient |
| `nugget-policy` | M4 | in use | Action descriptors, risk classes, the policy engine (rules per app and site, classifier, approvals, rate limits), confirmations, the kill switch, the hash-chained action log |
| `nugget-agent` | M4 | in use | The agent loop: tool registry, policy-gated actions, budgets, verify-after-act, idempotent action ids |
| `nugget-computer` | M4 | in use | Computer-use agent tools over `nugget-a11y`: observe a window as a bounded element tree, act by stable element id, verify every action by reading back, risk per target |
| `nugget-browser` | M4 | in use | Browser control through the extension in `extensions/browser` and the `nugget-browser-host` native-messaging host; per-site consent |
| `nugget-proactive` | M5 | being wired | Proactive cards: deterministic generators, a feedback-learned score, an optional judge, typed actions with dry-run diffs, the morning brief |
| `nugget-plugin` | M6 | in use | WASM component plugins: declared, user-approved capabilities enforced by the host, resource limits, signed packages, a guest SDK |
| `nugget-connector-microsoft` | M6 | in use | `outlook`, `outlook-calendar` and `onedrive` through Microsoft Graph delta queries |
| `nugget-connector-saas` | M6 | in use | `slack`, `notion`, `github` and `linear` through their official APIs |
| `nugget-desktop` | M0 | in use | The Tauri shell (`apps/desktop`): typed commands proxying IPC, the React pages, the in-app updater |
| `nugget-bench` | M0 | in use | The benchmark harness, suites and report schema (`bench/`); see [Benchmarks](benchmarks.md) |

## Rules that hold across crates

- **Contracts first.** Changes to `nugget-core` need agreement from every crate owner.
- **No hard-coded model ids** in logic: work is routed by tier.
- **No `unsafe`**, except scoped, commented FFI.
- **Errors** convert to `nugget_core::Error` at crate boundaries, and library code does not panic on
  bad input.
- **Every network request** goes through a metered client that records egress and never follows
  cross-host redirects.
