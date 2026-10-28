# Architecture overview

Nugget is one per-user daemon, `nuggetd`, and the apps that talk to it. The daemon owns the encrypted
store, the connectors, every model call and the MCP server. The desktop app holds the OS permissions
the daemon should not have (for Double Tap: accessibility and the global hotkey) and talks to the
daemon over the [local IPC protocol](../reference/ipc.md). The browser extension reaches the daemon
through a small native-messaging host, `nugget-browser-host`.

> **Status:** milestones M0 to M2 run in the daemon today: ingest, the encrypted store, local and
> bring-your-own-key models, search and Ask with citations, the knowledge graph and vault, and the MCP
> server. Double Tap, the agent and its safety policy, the browser bridge, proactive cards, plugins and
> the Microsoft 365 and SaaS connectors are being wired in (wave W). They are drawn with dashed lines
> below and are not available yet.

## Diagram

The diagram is a Mermaid chart. It renders on GitHub
([view it there](https://github.com/rayhankhilji/nugget/blob/main/docs/book/src/architecture/overview.md));
this book shows its source. Solid lines run today; dashed lines are being wired in (wave W).

```mermaid
flowchart LR
  subgraph ext_src["Sources"]
    files["Folders, Obsidian"]
    google["Gmail, Google Calendar"]
    imapcal["IMAP, CalDAV servers"]
    msft["Microsoft Graph<br/>(Outlook, calendar, OneDrive)"]
    saas["Slack, Notion, GitHub, Linear"]
  end

  subgraph desktop["Desktop app (Tauri 2)"]
    pages["Pages: Chat, Search, Knowledge,<br/>Connected apps, Sources, Models,<br/>Privacy, Status"]
    overlay["Double Tap overlay<br/>nugget-hotkey, nugget-a11y"]
    wpages["Agent, Safety, Today,<br/>Plugins pages"]
  end

  subgraph nuggetd["nuggetd"]
    ipc["IPC handler<br/>nugget-ipc"]
    feat["features seam<br/>features/mod.rs"]
    conn["connectors<br/>nugget-connector, nugget-oauth"]
    ingest["ingest<br/>nugget-ingest, nugget-extract"]
    store[("store: SQLCipher,<br/>FTS5, sqlite-vec,<br/>egress ledger, audit log")]
    know["knowledge<br/>nugget-knowledge, nugget-graph"]
    vault["vault<br/>nugget-vault"]
    retr["retrieve<br/>nugget-retrieve"]
    infer["infer router<br/>nugget-infer"]
    rt["runtime<br/>nugget-runtime, nugget-models"]
    mcp["MCP server<br/>nugget-mcp<br/>stdio bridge + loopback HTTP"]
    draft["draft<br/>nugget-draft, nugget-voice"]
    agent["agent + policy<br/>nugget-agent, nugget-policy"]
    pro["proactive<br/>nugget-proactive"]
    plug["plugins<br/>nugget-plugin (WASM)"]
  end

  llama["llama-server<br/>(loopback)"]
  cloud["Cloud providers<br/>(your key)"]
  mcpc["MCP clients<br/>(AI apps)"]
  vdir["Vault folder<br/>(Markdown)"]
  bhost["Browser extension +<br/>nugget-browser-host"]

  files --> conn
  google --> conn
  imapcal --> conn
  msft -.-> conn
  saas -.-> conn
  conn --> ingest --> store
  store --> know --> store
  know --> vault <--> vdir
  store --> retr
  retr --> ipc
  ingest --> infer
  know --> infer
  retr --> infer
  infer --> rt --> llama
  infer -- "egress ledger" --> cloud
  pages <--> ipc
  mcpc <--> mcp
  mcp --> retr
  mcp --> know
  ipc --> feat
  feat -.-> draft
  feat -.-> agent
  feat -.-> pro
  feat -.-> plug
  draft -.-> retr
  agent -.-> infer
  pro -.-> store
  plug -.-> conn
  overlay -.-> ipc
  wpages -.-> ipc
  bhost -.-> ipc
```

## Walk-through

1. **Sources to connectors.** Each configured source in `sources.toml` gets a connector that turns it
   into a stream of upserts and deletes with versions. Files and Obsidian watch the file system; Gmail,
   Google Calendar, IMAP and CalDAV use the provider's own delta mechanism. Connectors never touch the
   store. OAuth sources use your own client through `nugget-oauth`.
2. **Ingest.** `nugget-ingest` runs one supervisor per source and a pipeline per change: extract,
   PII scan, optional local triage, chunk, embed, store. The cursor is saved only after everything
   before it is stored. See [Data flow](data-flow.md).
3. **Store.** One SQLCipher database holds items, chunks, the FTS5 and `sqlite-vec` indexes, the egress
   ledger, the hash-chained audit log and the knowledge-graph tables. The key is in the OS keychain.
4. **Knowledge.** After ingest commits, the daemon's knowledge task extracts entities and claims
   (every claim with an evidence quote), resolves entities and writes the graph. The vault re-renders
   the changed pages and merges your edits back. Changes go out as `knowledge.changed`.
5. **Retrieve.** Search and Ask use hybrid full-text and vector search with rank fusion and MMR, and
   pack passages under a token budget with `[n]` citations. Sensitive items are left out of anything
   sent to a cloud model.
6. **Models.** `nugget-infer` routes each request by capability tier to a local provider, the managed
   `llama-server` (installed from the signed manifest by `nugget-models`, supervised by
   `nugget-runtime`), or a cloud provider. Every non-local request writes an egress row.
7. **Serving.** The desktop app and the `nuggetd` CLI call the IPC handler. AI apps reach the
   [MCP server](../guide/mcp.md) over stdio (`nuggetd mcp --client <id>`, which relays over the same
   IPC socket) or streamable HTTP on 127.0.0.1, each with its own scoped token.
8. **Features (wave W).** New subsystems plug into `crates/nuggetd/src/features/`: one module each,
   with its own IPC methods, state, config section and shutdown, matched after the built-in methods.
   No feature is registered yet. Coming: Double Tap drafting, the agent with the policy engine and
   browser bridge, proactive cards and the morning brief, and WASM plugins. Their crates exist and are
   tested; the [crate map](crates.md) gives their state.

## Crates by layer

| Layer | Crates | Runs today |
|---|---|---|
| Contracts and plumbing | `nugget-core`, `nugget-ipc` | Yes |
| Storage | `nugget-store`, `nugget-graph` | Yes |
| Models | `nugget-infer`, `nugget-models`, `nugget-runtime` | Yes |
| Sources and ingest | `nugget-connector`, `nugget-oauth`, `nugget-extract`, `nugget-ingest`, `nugget-connector-files`, `nugget-connector-google`, `nugget-connector-imap`, `nugget-connector-caldav` | Yes |
| Knowledge | `nugget-knowledge`, `nugget-vault` | Yes |
| Serving | `nugget-retrieve`, `nugget-mcp` | Yes |
| Double Tap | `nugget-draft`, `nugget-voice`, `nugget-hotkey`, `nugget-a11y` | Being wired in |
| Agent and safety | `nugget-agent`, `nugget-policy`, `nugget-browser` | Being wired in |
| Proactive | `nugget-proactive` | Being wired in |
| Extensions and more sources | `nugget-plugin`, `nugget-connector-microsoft`, `nugget-connector-saas` | Being wired in |
| Binaries | `nuggetd`, `nugget-desktop`, `nugget-bench` | Yes |

Dependency rules that hold across crates: every crate depends on `nugget-core`; only `nuggetd` (and
the benchmark) depends on the M0 to M2 crates together; the desktop app depends only on `nugget-core`
and `nugget-ipc`, so it never opens the store or calls a model itself. Connectors depend on
`nugget-connector`, never on the store.
