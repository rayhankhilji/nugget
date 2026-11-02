# Architecture overview

Heed is one per-user daemon, `nuggetd`, and the apps that talk to it. The daemon owns the encrypted
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

Connectors emit versioned changes and never touch the store; ingest stores them in one SQLCipher
database; knowledge extraction and the vault follow; retrieval packs cited context; `nugget-infer`
routes every model call by tier and ledgers non-local requests ([Data flow](data-flow.md)). Subsystems
after M2 are modules in `crates/nuggetd/src/features/` with their own IPC methods and config. The desktop app depends only on `nugget-core` and `nugget-ipc`, so it never opens
the store or calls a model. See the [crate map](crates.md) and [security model](security.md).
