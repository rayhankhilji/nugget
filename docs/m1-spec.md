# M1: Memory spec

**Goal.** Nugget continuously learns from your files, notes, mail and calendar. Everything is searchable, with citations,
**within 60 s of a change**, and runs fully locally when you want it to.

**Exit criteria** (PLAN.md §5):
- `bench memory --backend real` shows freshness p95 of 60 s or less;
- retrieval recall@10 is at or above the M0 fake-index baseline on the synthetic corpus;
- all connectors pass their recorded-fixture tests;
- CI is green.

The M0 ground rules in `docs/m0-spec.md` still apply: ownership, scoped builds, no git, clean room, no `unsafe` (except
scoped FFI), mocked network in tests, cross-platform `cfg`, and MSRV 1.88. Also:
- **Egress.** Every connector that touches the network records `EgressEvent`s through `ConnectorContext::egress`, with feature `sync:<kind>`.
- **Redirects.** HTTP clients must not follow cross-host redirects.
- **Secrets.** Tokens and passwords live only in the OS keychain through `nugget_core::secrets`, or come from `env:VAR`. Never in config files or logs.
- **Hostile input.** Every parser treats input as hostile: size caps, timeouts, no panics. Use fuzz-style tests on malformed input.

## Architecture
```
connector (sync / watch) ──Change──▶ nugget-ingest pipeline ──▶ nugget-store
   files · obsidian · gmail · gcal · imap · caldav      │
                                                        ├─ extract (nugget-extract): bytes → text + structure
                                                        ├─ PII scan (deterministic) → triage (Tier::Triage, optional)
                                                        ├─ chunk (nugget-extract::chunk)
                                                        └─ embed (Tier::Embedding, optional; local llama-server via nugget-runtime)
query ──▶ nugget-retrieve (hybrid + MMR + packing with citations) ──▶ nuggetd IPC ──▶ desktop
```

## Shared contracts (written by the orchestrator before wave 1)
- **`nugget-connector`** crate: the `Connector` trait, `Change`, `Document`, `Content`, `ChangeSink` (with `checkpoint`), `ConnectorContext` (paths, egress sink, known item versions, secret access) and a `ConnectorFactory` registry.
- **`nugget-store` additions:** an item `version` column, `delete_source`, `item_versions(source)`, `get_chunks(ids)`, `source_counts`.
- **`nugget_core::ipc` additions:** `sources.*` and `ingest.status` methods, and the `ingest.progress` notification.
- **`nugget_core::secrets`:** `oauth_account`, `password_account`.

## Work packages, wave 1 (parallel)
| WP | Crate(s) | Summary |
|---|---|---|
| **M1-A** | `nugget-extract` | Bytes or path plus MIME → `Extracted { title, text, sections (heading paths), links, tags, metadata }`. Formats: txt, md (frontmatter, wikilinks, tags), code, html, pdf, docx, xlsx/csv, eml/MIME, ics, images (OCR via `ocrs` when its models are present). Structure-aware **chunker** with deterministic chunk boundaries |
| **M1-B** | `nugget-ingest` | Source registry (`<config>/sources.toml`), per-source supervisor (sync, watch, backoff), the pipeline (extract with timeout, PII scan, optional triage, chunk, batched embedding, store), cursor checkpointing ordered after persistence, power-aware scheduling, freshness metrics, an event stream |
| **M1-C** | `nugget-connector-files` | Files and **Obsidian** connectors: roots, include/exclude globs, `.gitignore`/`.nuggetignore`, a size cap, symlink policy, initial scan reconciled with `item_versions`, plus a `notify` watcher (debounced) handling create, modify, rename and delete |
| **M1-D** | `nugget-oauth`, `nugget-connector-google` | OAuth 2.0 PKCE loopback flow with a user-supplied client id, tokens in the keychain, refresh. **Gmail**: backfill window plus `history.list` deltas, with resync if the history id expires. **Google Calendar**: `syncToken` deltas, with resync on 410. Metered HTTP |
| **M1-E** | `nugget-connector-imap`, `nugget-connector-caldav` | IMAP over TLS: UIDVALIDITY/UID tracking, CONDSTORE when available, folder selection, app passwords from the keychain. CalDAV `sync-collection` (RFC 6578), falling back to ETags. Byte-metered egress |
| **M1-F** | `nugget-retrieve` | Query embedding (optional), over-fetched hybrid search, per-item grouping, MMR diversity, token-budgeted context packing with `[n]` citations (item, chunk, title, URL), and a retrieval-eval harness on fixtures |
| **M1-G** | `nugget-runtime` (+ the `nugget-models` manifest `runtimes` section) | Pinned, signed, SHA-verified **llama.cpp `llama-server`** binaries per OS and architecture. A supervisor for chat and embedding servers on loopback (random port, health check, restart with backoff, idle stop, GPU layers by hardware). Each running server is exposed as an `openai_compatible` `ProviderConfig` |
| **M1-H** | `apps/desktop` | A **Sources** page (add folder or Obsidian vault, connect Gmail/Calendar through OAuth, add IMAP/CalDAV, enable or disable, sync now, per-source status and errors); **Search** upgraded with citations and opening the source; an **Indexing** status widget with freshness, queue depth and errors |

## Wave 2
- **M1-I `nuggetd` integration.** Wire ingest, runtime and retrieve into the daemon, implement the new IPC methods, and auto-start local runtimes for the selected models. Run `bench memory --backend real` and record the results in `bench/README.md`.
- **Review.** Security and correctness reviewers, then fixes.

## Integration notes for wave 2 (collected from wave-1 reports)
- **Settings shapes.** The desktop wizard sends:
  - `files {roots, include, exclude}`
  - `obsidian {vault}`
  - `gmail`/`gcal {client_id}`
  - `imap {host, port, tls, username, folders}`
  - `caldav {url, username}`
  
  These must match each connector's `settings_schema`; reconcile them in the daemon or the UI.
- **Google client secret.** The UI sends it through `sources.secret.set`, never in settings. For OAuth kinds the daemon stores it under a dedicated account (`oauth_client_secret:<source>`), and the connector reads it from there.
- **`sources.changed` payload.** It is a single `SourceInfo`, and the UI also tolerates a list.
- After a secret is set, the UI calls `sources.sync`.
