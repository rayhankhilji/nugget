# Crate map

Nugget is one Cargo workspace. Shared contracts live in `nugget-core`, and every other crate builds
against them, which is what lets several people (and agents) build crates in parallel. A crate that is
a **contract** has its public types and traits fixed and tested, with the implementation still to
come; a **stub** has only a description.

```text
                          apps/desktop (Tauri 2: React + TypeScript)
                                     │ local IPC (nugget-ipc)
┌──────────────────────────────────── nuggetd ────────────────────────────────────────┐
│ connectors ─▶ nugget-ingest ─▶ nugget-store ─▶ nugget-retrieve ─▶ IPC handlers      │
│ (files, google, imap,  │ extract, PII,  │ SQLCipher, FTS5,   │ hybrid, MMR,         │
│  caldav; nugget-oauth) │ triage, chunk, │ sqlite-vec, ledger,│ cited packing        │
│                        │ embed          │ audit, graph       │                      │
│ nugget-infer (providers, router) ◀── nugget-runtime (llama-server) ◀── nugget-models │
└──────────────────────────────────────────────────────────────────────────────────────┘
   built, not wired in yet: nugget-knowledge ─▶ graph ─▶ nugget-vault, nugget-mcp
```

<!-- check-refs: crates -->
| Crate | Milestone | State | Responsibility |
|---|---|---|---|
| `nugget-core` | M0 | in use | Shared contracts: ids, errors, capability tiers and locality, egress events, paths, keychain naming, the IPC protocol types |
| `nugget-store` | M0 | in use | The SQLCipher store: items, chunks, FTS5 and `sqlite-vec` search with rank fusion, cursors, egress ledger, hash-chained audit log, knowledge-graph tables (`SqliteGraph`) |
| `nugget-infer` | M0 | in use | One `Provider` trait over Anthropic Messages, OpenAI Responses and OpenAI-compatible servers; locality classification; capability probe; tier router with fallback |
| `nugget-models` | M0 | in use | Signed model and runtime manifest, verified resumable downloads, content-addressed storage with rollback, hardware detection and recommendation |
| `nugget-ipc` | M0 | in use | JSON-RPC 2.0 over Unix sockets and Windows named pipes: server, client, limits, the single-daemon lock |
| `nuggetd` | M0 | in use | The daemon: config, startup and shutdown, IPC handlers, sources wiring, local runtime slots, service install |
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
| `nugget-graph` | M2 | built | Graph types, the `GraphStore` trait, the in-memory reference implementation and conformance tests |
| `nugget-knowledge` | M2 | built | Extraction (deterministic and LLM), entity resolution, claim lifecycle |
| `nugget-vault` | M2 | built | Markdown vault rendering, three-way merge, edits to graph changes, validators, watcher |
| `nugget-mcp` | M2 | built | The local MCP server: tools, resources, tokens, scopes, rate limits, audit, stdio and loopback HTTP |
| `nugget-a11y` | M3 | contract | Read the focused field and its thread; insert with read-back verification (AX, UI Automation, AT-SPI) |
| `nugget-hotkey` | M3 | contract | Global trigger: a double tap of a modifier, or a chord |
| `nugget-draft` | M3 | contract | Rule-based field classification; the drafting engine |
| `nugget-voice` | M3 | contract | The user's writing-style profile and exemplars |
| `nugget-policy` | M4 | contract | Action descriptors, risk classes, the policy gate, the kill switch |
| `nugget-agent` | M4 | stub | The agent loop with budgets and verify-after-act |
| `nugget-browser` | M4 | stub | Browser control through an extension and native messaging |
| `nugget-proactive` | M5 | stub | Candidate store, judge, cards, typed actions with dry-run diffs |
| `nugget-plugin` | M6 | stub | WASM component plugins with declared capabilities |
| `nugget-connector-microsoft` | M6 | stub | Outlook mail, calendar and OneDrive through Microsoft Graph |
| `nugget-connector-saas` | M6 | stub | Slack, Notion, GitHub and Linear |
| `nugget-desktop` | M0 | in use | The Tauri shell (`apps/desktop`): typed commands proxying IPC, the React pages |
| `nugget-bench` | M0 | in use | The benchmark harness and report schema (`bench/`) |

"in use" means the daemon or app runs it today; "built" means implemented and tested but not yet
wired into the daemon.

## Rules that hold across crates

- **Contracts first.** Changes to `nugget-core` need agreement from every crate owner.
- **No hard-coded model ids** in logic: work is routed by tier.
- **No `unsafe`**, except scoped, commented FFI.
- **Errors** convert to `nugget_core::Error` at crate boundaries, and library code does not panic on
  bad input.
- **Every network request** goes through a metered client that records egress and never follows
  cross-host redirects.
