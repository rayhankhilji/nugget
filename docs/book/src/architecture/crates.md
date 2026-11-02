# Crate map

One Cargo workspace. Shared contracts live in `nugget-core`; every crate below is wired into `nuggetd`
or the desktop app. See the [overview](overview.md) for how they fit together.

<!-- check-refs: crates -->
| Crate | Milestone | Responsibility |
|---|---|---|
| `nugget-core` | M0 | Shared contracts: ids, errors, capability tiers and locality, egress events, paths, keychain naming, the IPC protocol types |
| `nugget-store` | M0 | The SQLCipher store: items, chunks, FTS5 and `sqlite-vec` search with rank fusion, cursors, the item change feed, egress ledger, hash-chained audit log, knowledge-graph tables (`SqliteGraph`) |
| `nugget-infer` | M0 | One `Provider` trait over Anthropic Messages, OpenAI Responses and OpenAI-compatible servers; locality classification; capability probe; tier router with fallback; prompt caching and prewarm |
| `nugget-models` | M0 | Signed model and runtime manifest, verified resumable downloads, content-addressed storage with rollback, hardware detection and recommendation |
| `nugget-ipc` | M0 | JSON-RPC 2.0 over Unix sockets and Windows named pipes: server, client, limits, the single-daemon lock |
| `nuggetd` | M0 | The daemon: config, startup and shutdown, IPC handlers, sources wiring, local runtime slots, knowledge, vault and MCP wiring, `nuggetd mcp`, service install, the `features/` seam |
| `nugget-connector` | M1 | The connector API: `Connector`, `Change`, `ChangeSink`, `ConnectorContext`, `ConnectorFactory` registry, metered HTTP |
| `nugget-extract` | M1 | Bytes to text and structure (Markdown, HTML, PDF, DOCX, XLSX, CSV, MIME mail, iCalendar, code, OCR), and the structure-aware chunker |
| `nugget-ingest` | M1 | The ingest runtime: `sources.toml`, a supervisor per source, the pipeline (extract, PII scan, triage, chunk, embed, store), cursor-after-persistence, power policy, freshness metrics, re-embedding |
| `nugget-connector-files` | M1 | `files` and `obsidian`: scan, ignore files, native watcher with debounce, reconcile |
| `nugget-oauth` | M1 | OAuth 2.0 authorization code with PKCE and loopback redirect, token storage and refresh |
| `nugget-connector-google` | M1 | `gmail` (history deltas), `gcal` (sync tokens) and `gdrive` (Drive changes feed) |
| `nugget-connector-imap` | M1 | `imap`: TLS only, UIDVALIDITY, CONDSTORE and QRESYNC, IDLE |
| `nugget-connector-caldav` | M1 | `caldav`: discovery, RFC 6578 sync, ETag fallback |
| `nugget-retrieve` | M1 | Query embedding, over-fetched hybrid search, per-item grouping, MMR, token-budgeted packing with `[n]` citations, the sensitive filter, an eval harness |
| `nugget-runtime` | M1 | Supervised `llama-server` processes on loopback: install, spawn, health, restart, idle stop, per-start API keys |
| `nugget-graph` | M2 | Graph types, the `GraphStore` trait, the in-memory reference implementation and conformance tests |
| `nugget-knowledge` | M2 | Extraction (deterministic and LLM), entity resolution, claim lifecycle, run by the daemon after ingest |
| `nugget-vault` | M2 | Markdown vault rendering, three-way merge, edits to graph changes, validators, watcher |
| `nugget-mcp` | M2 | The local MCP server: read-only tools, resources, per-client tokens and scopes, rate limits, audit, stdio and loopback HTTP |
| `nugget-a11y` | M3 | Read the focused field and its thread; insert with read-back verification (macOS AX, Windows UI Automation, Linux AT-SPI); actions for computer use |
| `nugget-hotkey` | M3 | Global trigger: a double tap of a modifier, or a chord, on macOS, Windows and Linux; observes only the configured trigger |
| `nugget-draft` | M3 | Rule-based field classification, the secure-field and excluded-app gate, the drafting engine with budgeted, cited context |
| `nugget-voice` | M3 | The user's writing-style profile and exemplars per channel and recipient |
| `nugget-policy` | M4 | Action descriptors, risk classes, the policy engine (rules per app and site, classifier, approvals, rate limits), confirmations, the kill switch, the hash-chained action log |
| `nugget-agent` | M4 | The agent loop: tool registry, policy-gated actions, budgets, verify-after-act, idempotent action ids |
| `nugget-computer` | M4 | Computer-use agent tools over `nugget-a11y`: observe a window as a bounded element tree, act by stable element id, verify every action by reading back, risk per target |
| `nugget-browser` | M4 | Browser control through the extension in `extensions/browser` and the `nugget-browser-host` native-messaging host; per-site consent |
| `nugget-proactive` | M5 | Proactive cards: deterministic generators, a feedback-learned score, an optional judge, typed actions with dry-run diffs, the morning brief |
| `nugget-plugin` | M6 | WASM component plugins: declared, user-approved capabilities enforced by the host, resource limits, signed packages, a guest SDK |
| `nugget-connector-microsoft` | M6 | `outlook`, `outlook-calendar` and `onedrive` through Microsoft Graph delta queries |
| `nugget-connector-saas` | M6 | `slack`, `notion`, `github` and `linear` through their official APIs |
| `nugget-connector-messaging` | M6 | `imessage` (a copy of the macOS Messages database, address-book names), `whatsapp` ("Export chat" files, watched) and `telegram` (Desktop JSON exports, watched): one Markdown document per chat per month, private by default |
| `nugget-cloud` | Cloud M1 | The cloud service (`docs/architecture-cloud.md`): Supabase Postgres with per-user RLS, Composio-connected sync, the metered AI gateway, cited Ask, export and account deletion; knowledge, "Needs you" cards, the morning brief and hybrid search (M2/M3) by reusing `nugget-knowledge`, `nugget-proactive`, `nugget-ingest`'s rules and `nugget-retrieve`'s ranking over a per-user snapshot |
| `nugget-cloud-client` | Cloud M1 | Desktop side of the cloud: PKCE sign-in (magic link, Google), keychain-held refresh token, a typed `/v1` client |
| `nugget-desktop` | M0 | The Tauri shell (`apps/desktop`): typed commands proxying IPC, the React pages, the in-app updater |
| `nugget-bench` | M0 | The benchmark harness, suites and report schema (`bench/`); see [Benchmarks](benchmarks.md) |

Rules that hold across crates are in [Rules](../contributing/rules.md#tests-and-code).
