# Architecture overview

Nugget is one per-user daemon, `nuggetd`, and the apps that talk to it. The daemon owns the encrypted
store, the connectors, every model call and the MCP server. The desktop app holds the OS permissions
the daemon should not have (accessibility, the global hotkey) and talks to the daemon over the
[local IPC protocol](../reference/ipc.md). The browser extension reaches the daemon through the
native-messaging host `nugget-browser-host`.

```text
                    apps/desktop (Tauri 2) · browser extension · nuggetd CLI
                                     │ local IPC (nugget-ipc)
┌────────────────────────────────────── nuggetd ───────────────────────────────────────┐
│ connectors ─▶ nugget-ingest ─▶ nugget-store ─▶ nugget-retrieve ─▶ IPC handlers       │
│ (native, plugins)      │ extract, PII,  │ SQLCipher, FTS5,   │ hybrid, MMR,          │
│ nugget-oauth           │ triage, chunk, │ sqlite-vec, ledger,│ cited packing         │
│                        │ embed          │ audit, graph       │                       │
│ nugget-knowledge ─▶ graph ─▶ nugget-vault     nugget-mcp ◀── MCP apps (stdio, HTTP)  │
│ nugget-infer (router) ◀── nugget-runtime (llama-server) ◀── nugget-models            │
│ features/: draft, agent, policy, browser, proactive, plugins, backup, search UI      │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

1. **Connectors** turn each source in `sources.toml` into versioned upserts and deletes; they never
   touch the store.
2. **Ingest** extracts, scans for PII, triages locally, chunks, embeds and stores, saving the cursor
   only after the data ([Data flow](data-flow.md)).
3. **Store**: one SQLCipher database with indexes, egress ledger, audit log and graph.
4. **Knowledge** extracts cited entities and claims after ingest; the vault re-renders changed pages and
   merges your edits back.
5. **Retrieve**: hybrid search, MMR and budgeted packing with `[n]` citations; sensitive items never go
   to a cloud model.
6. **Models**: `nugget-infer` routes by tier to local servers, the managed `llama-server`, or cloud
   providers; every non-local request writes an egress row.
7. **Features**: subsystems after M2 live in `crates/nuggetd/src/features/`, each owning its IPC
   methods, tasks, config section and shutdown.

The desktop app depends only on `nugget-core` and `nugget-ipc`: it never opens the store or calls a
model itself. Connectors depend on `nugget-connector`, never on the store. See the
[crate map](crates.md).
